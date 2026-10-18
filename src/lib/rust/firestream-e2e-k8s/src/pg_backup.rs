//! PostgreSQL backup/restore round-trip e2e (Phase 3 of the pg-backup
//! feature).
//!
//! This is the only multi-chart test in the k8s harness: the per-chart
//! sweep (`harness::run_one`) deploys one chart in isolation and so
//! cannot prove the S3 round-trip. Here we:
//!
//!  1. Stand up a fresh k3d cluster (same primitives as `run_one`).
//!  2. Deploy **seaweedfs** first (the in-cluster S3 object store; its
//!     post-install hook creates the `firestream` bucket), then
//!     **postgresql** with `backup.enabled=true` (the chart ships it
//!     `false`, so we flip it via the harness's `--set` override seam in
//!     [`crate::deploy::deploy_chart_with_overrides`]).
//!  3. Seed a sentinel row via `kubectl exec … psql`.
//!  4. Trigger an on-demand backup by driving the SAME `KubectlClient`
//!     helpers the `firestream helm backup` CLI arm uses
//!     (`create_job_from_cronjob` + `wait_for_job` + `get_logs`), and
//!     scrape the produced object key from the job logs
//!     (`backup complete: <key>`).
//!  5. Assert the object exists (`aws s3 ls` exec'd inside the pg pod,
//!     which carries the awscli2 baked into the firestream-postgresql
//!     image).
//!  6. Drop the table (simulate data loss).
//!  7. Restore by applying the SAME one-shot Job manifest the CLI
//!     `restore` arm renders, then `wait_for_job`.
//!  8. Assert the sentinel row is back.
//!  9. Teardown via the RAII guards (skipped under `FIRESTREAM_E2E_K8S_KEEP=1`).
//!
//! Gated behind the existing `FIRESTREAM_E2E_K8S_*` contract; local-only
//! (NOT added to CI). The filter token is `pg-backup`
//! (`FIRESTREAM_E2E_K8S_STACKS=pg-backup`).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use firestream::cli::commands::{build_restore_job_json, restore_command};
use firestream_e2e_core::exec::Exec;
use firestream_e2e_core::k8s::exec::KubectlExec;
use firestream_e2e_core::k8s::{cluster, probes, readiness};
use firestream_e2e_core::retry::retry_until_sync;
use helm_manager::kubectl_client::KubectlClient;

use crate::deploy::{self, DeployedRelease};
use crate::env::{env_keep_k8s, env_strict_k8s, env_timeout_secs_k8s, selected_k8s};
use crate::harness::{ReleaseGuard, harness_lock, resolve_charts_dir, should_skip_k8s};

/// Filter token for `FIRESTREAM_E2E_K8S_STACKS`. Not a chart name — this
/// is a composite (seaweedfs + postgresql) scenario, so it gets its own
/// gate key.
const FILTER: &str = "pg-backup";
const CHART_S3: &str = "seaweedfs";
const CHART_PG: &str = "postgresql";

/// Sentinel table + row probed across the backup/restore boundary.
const SEED_SQL: &str = "CREATE TABLE IF NOT EXISTS firestream_e2e_backup (id int primary key, val text); \
     INSERT INTO firestream_e2e_backup (id, val) VALUES (1, 'firestream-sentinel') \
     ON CONFLICT (id) DO UPDATE SET val = EXCLUDED.val;";
const DROP_SQL: &str = "DROP TABLE IF EXISTS firestream_e2e_backup;";
const SELECT_SQL: &str = "SELECT val FROM firestream_e2e_backup WHERE id = 1;";
const SENTINEL: &str = "firestream-sentinel";

/// Run the full backup/restore round-trip. Panics on any failure
/// (matching `harness::run_one`'s contract; `#[test]` consumers expect
/// panic-on-failure semantics).
pub fn run_pg_backup_roundtrip() {
    // ---- 1. skip gate ----
    if let Some(reason) = should_skip_k8s() {
        if env_strict_k8s() {
            panic!("[e2e-k8s:{}] STRICT=1 and prerequisite missing: {}", FILTER, reason);
        }
        eprintln!("[e2e-k8s:{}] SKIP: {}", FILTER, reason);
        return;
    }

    // ---- 2. filter gate ----
    if !selected_k8s(FILTER) {
        eprintln!(
            "[e2e-k8s:{}] SKIP: not in FIRESTREAM_E2E_K8S_STACKS (set FIRESTREAM_E2E_K8S_STACKS={} to run)",
            FILTER, FILTER
        );
        return;
    }

    // ---- 3. process-wide mutex ----
    let _guard = harness_lock().lock().unwrap_or_else(|p| p.into_inner());

    // ---- 4. cluster ----
    let handle = match cluster::create_cluster(FILTER) {
        Ok(h) => h,
        Err(e) => panic!("[e2e-k8s:{}] cluster create failed: {:#}", FILTER, e),
    };
    eprintln!(
        "[e2e-k8s:{}] cluster up: name={} kubeconfig={}",
        FILTER,
        handle.name,
        handle.kubeconfig.display()
    );
    let _cluster_guard = cluster::ClusterGuard::arm(handle.clone(), env_keep_k8s());

    let charts_dir = resolve_charts_dir(FILTER);

    // Single tokio runtime for every async KubectlClient call below. Built
    // once and reused; dropped at scope exit, well before any guard unwind.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio current-thread runtime");
    let kubectl = KubectlClient::with_config(None, Some(handle.kubeconfig.clone()))
        .expect("construct KubectlClient (kubectl on PATH)");

    // ---- 5a. seaweedfs first (object store + bucket hook) ----
    let s3 = deploy::deploy_chart(&handle, &charts_dir, CHART_S3)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] seaweedfs deploy failed: {:#}", FILTER, e));
    let _s3_guard = ReleaseGuard::arm(s3.clone(), handle.kubeconfig.clone(), env_keep_k8s());
    wait_and_probe(&handle, &s3, CHART_S3);
    eprintln!("[e2e-k8s:{}] seaweedfs ready (bucket created by post-install hook)", FILTER);

    // ---- 5b. postgresql with backup enabled ----
    let pg =
        deploy::deploy_chart_with_overrides(&handle, &charts_dir, CHART_PG, &[("backup.enabled", "true")])
            .unwrap_or_else(|e| panic!("[e2e-k8s:{}] postgresql deploy failed: {:#}", FILTER, e));
    let _pg_guard = ReleaseGuard::arm(pg.clone(), handle.kubeconfig.clone(), env_keep_k8s());
    wait_and_probe(&handle, &pg, CHART_PG);
    eprintln!("[e2e-k8s:{}] postgresql ready (backup.enabled=true)", FILTER);

    // Resolve names the way the CLI does.
    let fullname = bitnami_fullname(&pg.release, &pg.chart);
    let ns = &pg.namespace;
    let secret_name = fullname.clone();
    let password = read_secret_value(&handle.kubeconfig, ns, &secret_name, "postgres-password")
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read postgres-password secret: {:#}", FILTER, e));

    let exec = KubectlExec {
        namespace: ns.clone(),
        kubeconfig: handle.kubeconfig.clone(),
    };
    let pod = resolve_pg_pod(&handle.kubeconfig, &pg.release, ns)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] resolve pg pod: {:#}", FILTER, e));

    // ---- 6. seed sentinel row ----
    psql(&exec, &pod, &password, SEED_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] seed: {:#}", FILTER, e));
    let seeded = psql(&exec, &pod, &password, SELECT_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read-back seed: {:#}", FILTER, e));
    assert!(
        seeded.contains(SENTINEL),
        "[e2e-k8s:{}] sentinel not present after seed; got {:?}",
        FILTER, seeded
    );
    eprintln!("[e2e-k8s:{}] seeded sentinel row", FILTER);

    // ---- 7. backup (mirror `firestream helm backup` CLI arm) ----
    let job_timeout = env_timeout_secs_k8s();
    let cronjob = format!("{}-pgdumpall", fullname);
    let backup_job = k8s_name_trunc(format!("{}-manual-{}", cronjob, short_id()));
    rt.block_on(kubectl.create_job_from_cronjob(ns, &cronjob, &backup_job))
        .unwrap_or_else(|e| {
            panic!(
                "[e2e-k8s:{}] create backup job from cronjob `{}` (is backup.enabled honored?): {}",
                FILTER, cronjob, e
            )
        });
    rt.block_on(kubectl.wait_for_job(ns, &backup_job, job_timeout))
        .unwrap_or_else(|e| {
            let logs = rt
                .block_on(kubectl.get_logs(ns, &format!("job/{}", backup_job)))
                .unwrap_or_else(|le| format!("(could not fetch backup job logs: {})", le));
            panic!(
                "[e2e-k8s:{}] backup job did not complete: {}\n--- backup job logs ({}) ---\n{}",
                FILTER, e, backup_job, logs
            )
        });

    let logs = rt
        .block_on(kubectl.get_logs(ns, &format!("job/{}", backup_job)))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read backup job logs: {}", FILTER, e));
    let full_key = logs
        .lines()
        .rev()
        .find_map(|l| l.split("backup complete:").nth(1).map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "[e2e-k8s:{}] could not find `backup complete: <key>` in backup job logs:\n{}",
                FILTER, logs
            )
        });
    eprintln!("[e2e-k8s:{}] backup produced object: {}", FILTER, full_key);

    // ---- 8. assert the object exists in SeaweedFS ----
    aws_s3_ls(&exec, &pod, &full_key)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] object not found in object store: {:#}", FILTER, e));
    eprintln!("[e2e-k8s:{}] confirmed object present via `aws s3 ls`", FILTER);

    // ---- 9. drop the table (simulate data loss) ----
    psql(&exec, &pod, &password, DROP_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] drop table: {:#}", FILTER, e));
    eprintln!("[e2e-k8s:{}] dropped sentinel table", FILTER);

    // ---- 10. restore (mirror `firestream helm restore` CLI arm) ----
    // The CLI strips the `s3://<bucket>/` prefix and passes the remainder
    // as `--from`; the restore Job re-prepends bucket + endpoint.
    let from = full_key
        .strip_prefix("s3://firestream/")
        .unwrap_or(&full_key)
        .to_string();
    // Inherit the deployed CronJob's container spec via the SAME shared builder
    // the CLI restore arm uses (`firestream::cli::commands`), so the e2e and the
    // CLI can never drift and the cloud-S3 / SeaweedFS env contract is honored.
    let cronjob_json = rt
        .block_on(kubectl.get_resource_json("cronjob", &cronjob, ns))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] get cronjob `{}` json: {}", FILTER, cronjob, e));
    let restore_job = k8s_name_trunc(format!("{}-pgrestore-{}", fullname, short_id()));
    let job_manifest =
        build_restore_job_json(&cronjob_json, &restore_job, ns, &from, &restore_command(CHART_PG))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] build restore job manifest: {}", FILTER, e));
    rt.block_on(kubectl.apply_yaml(&job_manifest, Some(ns)))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] apply restore job: {}", FILTER, e));
    rt.block_on(kubectl.wait_for_job(ns, &restore_job, job_timeout))
        .unwrap_or_else(|e| {
            let logs = rt
                .block_on(kubectl.get_logs(ns, &format!("job/{}", restore_job)))
                .unwrap_or_else(|le| format!("(could not fetch restore job logs: {})", le));
            panic!(
                "[e2e-k8s:{}] restore job did not complete: {}\n--- restore job logs ({}) ---\n{}",
                FILTER, e, restore_job, logs
            )
        });
    eprintln!("[e2e-k8s:{}] restore job completed from key `{}`", FILTER, from);

    // ---- 11. assert the sentinel row is back ----
    let restored = psql(&exec, &pod, &password, SELECT_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read-back after restore: {:#}", FILTER, e));
    assert!(
        restored.contains(SENTINEL),
        "[e2e-k8s:{}] sentinel NOT restored; SELECT returned {:?}",
        FILTER, restored
    );

    eprintln!("[e2e-k8s:{}] round-trip OK: seed -> backup -> drop -> restore -> assert", FILTER);
    // Guards drop in reverse declaration order: pg release, s3 release,
    // then the cluster. Namespace deletes reap the backup/restore Jobs.
}

/// Wait for a release's pods to be Ready, then run its readiness probe
/// chain. Mirrors the wait+probe phase of `harness::run_one` for a single
/// chart, with a fresh per-chart deadline.
fn wait_and_probe(handle: &cluster::ClusterHandle, rel: &DeployedRelease, chart: &str) {
    let deadline = Instant::now() + Duration::from_secs(env_timeout_secs_k8s());
    readiness::wait_pods_ready(&handle.kubeconfig, &rel.release, &rel.namespace, deadline)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] wait_pods_ready({}): {:#}", FILTER, chart, e));
    let ctx = probes::K8sCtx {
        handle: handle.clone(),
        namespace: rel.namespace.clone(),
        release: rel.release.clone(),
        deadline,
    };
    let chain = probes::for_chart(chart, &ctx)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] probe chain for {}: {:#}", FILTER, chart, e));
    for probe in chain {
        retry_until_sync(&deadline, || probe.run(&ctx)).unwrap_or_else(|e| {
            panic!("[e2e-k8s:{}] probe {} ({}) failed: {:#}", FILTER, probe.name(), chart, e)
        });
    }
}

/// Resolve a concrete pg pod name via the Bitnami instance label.
fn resolve_pg_pod(kubeconfig: &Path, release: &str, namespace: &str) -> Result<String> {
    let selector = format!("app.kubernetes.io/instance={}", release);
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args([
            "get", "pods", "-l", &selector, "-n", namespace, "-o",
            "jsonpath={.items[0].metadata.name}",
        ])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get pods")?;
    if !out.status.success() {
        bail!(
            "kubectl get pods (release={} ns={}): {}",
            release,
            namespace,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        bail!("no pods for selector `{}` in ns `{}`", selector, namespace);
    }
    Ok(name)
}

/// Run a SQL statement inside the pg pod over a password-authenticated TCP
/// connection (forces md5/scram auth, which the mounted secret satisfies).
/// Returns stdout on success; errors with captured stderr on a non-zero
/// psql exit.
fn psql(exec: &KubectlExec, pod: &str, password: &str, sql: &str) -> Result<String> {
    let pgpass = format!("PGPASSWORD={}", password);
    let out = exec
        .exec(
            pod,
            &[
                "env", &pgpass, "psql", "-h", "127.0.0.1", "-p", "5432", "-U", "postgres",
                "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-tAc", sql,
            ],
        )
        .context("kubectl exec psql")?;
    if !out.status.success() {
        bail!(
            "psql exit {}: stderr {} stdout {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `aws s3 ls <full s3:// key>` inside the pg pod (awscli2 is baked into
/// the firestream-postgresql image). Passes creds + endpoint explicitly
/// since the SeaweedFS Secret lives cross-namespace. Asserts a non-empty,
/// successful listing for the exact key.
fn aws_s3_ls(exec: &KubectlExec, pod: &str, full_key: &str) -> Result<()> {
    let out = exec
        .exec(
            pod,
            &[
                "env",
                "AWS_ACCESS_KEY_ID=firestream",
                "AWS_SECRET_ACCESS_KEY=firestream-secret",
                "AWS_DEFAULT_REGION=us-east-1",
                "aws", "s3", "ls", full_key, "--endpoint-url",
                "http://seaweedfs-all-in-one.seaweedfs.svc.cluster.local:8333",
            ],
        )
        .context("kubectl exec aws s3 ls")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || stdout.trim().is_empty() {
        bail!(
            "aws s3 ls {} returned no object (exit {}): stderr {} stdout {}",
            full_key,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
            stdout.trim()
        );
    }
    Ok(())
}

/// Read a Secret value: `kubectl get secret … -o jsonpath` then decode the
/// base64 with the host `base64` binary (coreutils — avoids pulling a new
/// crate dep just to read one secret).
fn read_secret_value(
    kubeconfig: &Path,
    namespace: &str,
    secret: &str,
    key: &str,
) -> Result<String> {
    let jsonpath = format!("jsonpath={{.data.{}}}", key);
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["get", "secret", secret, "-n", namespace, "-o", &jsonpath])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get secret")?;
    if !out.status.success() {
        bail!(
            "kubectl get secret {}/{}: {}",
            namespace,
            secret,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let b64 = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b64.is_empty() {
        bail!("secret {}/{} key `{}` is empty", namespace, secret, key);
    }
    base64_decode_via_cli(&b64)
}

/// Pipe a base64 string through `base64 -d` and return the decoded UTF-8.
fn base64_decode_via_cli(b64: &str) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("base64")
        .arg("-d")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn base64 -d")?;
    child
        .stdin
        .take()
        .context("base64 stdin")?
        .write_all(b64.as_bytes())
        .context("write base64 input")?;
    let out = child.wait_with_output().context("wait base64 -d")?;
    if !out.status.success() {
        bail!("base64 -d failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
}

/// Replicate Bitnami's `common.names.fullname` for the common no-override
/// case (same logic as the CLI's `bitnami_fullname`): if the release name
/// already contains the chart name, return it unchanged; otherwise
/// `<release>-<chart>`. Truncated to 63 chars.
fn bitnami_fullname(release_name: &str, chart_name: &str) -> String {
    let base = if release_name.contains(chart_name) {
        release_name.to_string()
    } else {
        format!("{}-{}", release_name, chart_name)
    };
    k8s_name_trunc(base)
}

/// Trim a name to the 63-char k8s limit, dropping a trailing `-`.
fn k8s_name_trunc(name: String) -> String {
    let mut s: String = name.chars().take(63).collect();
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// Short, collision-resistant suffix for ad-hoc Job names. Derived from
/// the wall clock — no extra crate dep; one test run never collides.
fn short_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let hex = format!("{:x}", nanos);
    hex.chars().rev().take(8).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullname_postgresql_standalone() {
        // Release name contains the chart name -> not suffixed.
        assert_eq!(bitnami_fullname("postgresql", "postgresql"), "postgresql");
        // Release name without chart name -> `<release>-<chart>`.
        assert_eq!(bitnami_fullname("testpg", "postgresql"), "testpg-postgresql");
    }

    #[test]
    fn short_id_is_short_hex() {
        let id = short_id();
        assert!(id.len() <= 8, "got {:?}", id);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "got {:?}", id);
    }

    #[test]
    fn restore_job_inherits_via_shared_builder() {
        // The restore manifest is built by the shared
        // `firestream::cli::commands::build_restore_job_json` (single source
        // of truth with the CLI). Smoke-test the e2e's wiring of it here; the
        // exhaustive field-inheritance contract is pinned by that crate's own
        // unit tests.
        let cronjob_json = r#"{
            "kind": "CronJob",
            "spec": { "jobTemplate": { "spec": { "template": { "spec": {
                "containers": [{
                    "name": "x-pgdumpall",
                    "image": "registry/firestream-postgresql:1",
                    "command": ["bash", "-c", "dump"],
                    "env": [{ "name": "PGHOST", "value": "postgresql" }]
                }],
                "volumes": []
            }}}}}
        }"#;
        let out = build_restore_job_json(
            cronjob_json,
            "postgresql-pgrestore-abc",
            "postgresql",
            "pg-backups/pg_dumpall-2026-01-01-00-00-00.sql.gz",
            &restore_command("postgresql"),
        )
        .expect("build restore job");
        assert!(out.contains("\"kind\": \"Job\""));
        assert!(out.contains("registry/firestream-postgresql:1"));
        assert!(out.contains("S3_BACKUP_KEY"));
        assert!(out.contains("pg-backups/pg_dumpall-2026-01-01-00-00-00.sql.gz"));
    }
}
