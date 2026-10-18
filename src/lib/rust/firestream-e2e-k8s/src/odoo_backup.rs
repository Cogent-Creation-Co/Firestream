//! Odoo backup/restore round-trip e2e.
//!
//! Sister of [`crate::pg_backup`]. Multi-chart, so it cannot ride the
//! per-chart sweep (`harness::run_one`):
//!
//!  1. Stand up a fresh k3d cluster.
//!  2. Deploy **seaweedfs** (its post-install hook creates the `firestream`
//!     bucket), then **odoo** with `backup.enabled=true` via the harness's
//!     `--set` override seam.
//!  3. Seed a sentinel row in the Odoo database AND a sentinel file in the
//!     filestore (`$ODOO_DATA_DIR/filestore/<db>/`), both via `kubectl exec`
//!     into the odoo pod using the image's own `odoo_pg` helper.
//!  4. Trigger an on-demand backup through the SAME `KubectlClient` calls
//!     the `firestream helm backup` CLI arm uses and scrape the object key
//!     from the job logs (`backup complete: <key>`).
//!  5. Assert the object exists (`aws s3 ls` inside the odoo pod, which
//!     carries awscli2).
//!  6. Drop the table and delete the file.
//!  7. Restore through [`firestream::cli::commands::restore_from_backup`],
//!     the exact function behind `firestream helm restore odoo`. Odoo's
//!     manifest sets `quiesceDeployment`, so this scales the Deployment to
//!     zero, runs the restore Job against the freed PVC, scales back, and
//!     waits for the rollout.
//!  8. Assert the row and the file are back on the fresh pod.
//!  9. Teardown via the RAII guards (skipped under `FIRESTREAM_E2E_K8S_KEEP=1`).
//!
//! Gated behind the `FIRESTREAM_E2E_K8S_*` contract; filter token
//! `odoo-backup` (`FIRESTREAM_E2E_K8S_STACKS=odoo-backup`).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use firestream::cli::commands::{BackupTarget, restore_command, restore_from_backup};
use firestream_charts::Charts;
use firestream_e2e_core::exec::Exec;
use firestream_e2e_core::k8s::exec::KubectlExec;
use firestream_e2e_core::k8s::{cluster, probes, readiness};
use firestream_e2e_core::retry::retry_until_sync;
use helm_manager::kubectl_client::KubectlClient;

use crate::deploy::{self, DeployedRelease};
use crate::env::{env_keep_k8s, env_strict_k8s, env_timeout_secs_k8s, selected_k8s};
use crate::harness::{ReleaseGuard, harness_lock, resolve_charts_dir, should_skip_k8s};

/// Filter token for `FIRESTREAM_E2E_K8S_STACKS`. A composite scenario
/// (seaweedfs + odoo), so it gets its own gate key.
const FILTER: &str = "odoo-backup";
const CHART_S3: &str = "seaweedfs";
const CHART_ODOO: &str = "odoo";

const SEED_SQL: &str = "CREATE TABLE IF NOT EXISTS firestream_e2e_backup (id int primary key, val text); \
     INSERT INTO firestream_e2e_backup (id, val) VALUES (1, 'firestream-sentinel') \
     ON CONFLICT (id) DO UPDATE SET val = EXCLUDED.val;";
const DROP_SQL: &str = "DROP TABLE IF EXISTS firestream_e2e_backup;";
const SELECT_SQL: &str = "SELECT val FROM firestream_e2e_backup WHERE id = 1;";
const SENTINEL: &str = "firestream-sentinel";
/// File name written under the database's filestore directory.
const SENTINEL_FILE: &str = "firestream-e2e-sentinel";

/// Run the full backup/restore round-trip. Panics on any failure (same
/// contract as `harness::run_one`).
pub fn run_odoo_backup_roundtrip() {
    if let Some(reason) = should_skip_k8s() {
        if env_strict_k8s() {
            panic!("[e2e-k8s:{}] STRICT=1 and prerequisite missing: {}", FILTER, reason);
        }
        eprintln!("[e2e-k8s:{}] SKIP: {}", FILTER, reason);
        return;
    }
    if !selected_k8s(FILTER) {
        eprintln!(
            "[e2e-k8s:{}] SKIP: not in FIRESTREAM_E2E_K8S_STACKS (set FIRESTREAM_E2E_K8S_STACKS={} to run)",
            FILTER, FILTER
        );
        return;
    }

    let _guard = harness_lock().lock().unwrap_or_else(|p| p.into_inner());

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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio current-thread runtime");
    let kubectl = KubectlClient::with_config(None, Some(handle.kubeconfig.clone()))
        .expect("construct KubectlClient (kubectl on PATH)");

    // ---- seaweedfs first (object store + bucket hook) ----
    let s3 = deploy::deploy_chart(&handle, &charts_dir, CHART_S3)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] seaweedfs deploy failed: {:#}", FILTER, e));
    let _s3_guard = ReleaseGuard::arm(s3.clone(), handle.kubeconfig.clone(), env_keep_k8s());
    wait_and_probe(&handle, &s3, CHART_S3);
    eprintln!("[e2e-k8s:{}] seaweedfs ready", FILTER);

    // ---- odoo with the backup CronJob rendered ----
    let odoo = deploy::deploy_chart_with_overrides(
        &handle,
        &charts_dir,
        CHART_ODOO,
        &[("backup.enabled", "true")],
    )
    .unwrap_or_else(|e| panic!("[e2e-k8s:{}] odoo deploy failed: {:#}", FILTER, e));
    let _odoo_guard = ReleaseGuard::arm(odoo.clone(), handle.kubeconfig.clone(), env_keep_k8s());
    wait_and_probe(&handle, &odoo, CHART_ODOO);
    eprintln!("[e2e-k8s:{}] odoo ready (backup.enabled=true)", FILTER);

    // Resolve names exactly the way the CLI does: from the chart manifest.
    let manifest = Charts::open(&charts_dir)
        .and_then(|c| c.get(CHART_ODOO))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read odoo manifest: {:#}", FILTER, e));
    let target = BackupTarget::from_manifest(&manifest, Some(odoo.namespace.clone()))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] resolve backup target: {}", FILTER, e));
    assert_eq!(
        target.release, odoo.release,
        "[e2e-k8s:{}] manifest release name must match the deployed release",
        FILTER
    );
    let ns = &target.namespace;
    let selector = target.app_pod_selector();

    let exec = KubectlExec {
        namespace: ns.clone(),
        kubeconfig: handle.kubeconfig.clone(),
    };
    let pod = resolve_pod(&handle.kubeconfig, &selector, ns)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] resolve odoo pod: {:#}", FILTER, e));

    // ---- seed: sentinel row + sentinel file ----
    psql(&exec, &pod, SEED_SQL).unwrap_or_else(|e| panic!("[e2e-k8s:{}] seed row: {:#}", FILTER, e));
    let seeded = psql(&exec, &pod, SELECT_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read-back seed: {:#}", FILTER, e));
    assert!(seeded.contains(SENTINEL), "[e2e-k8s:{}] sentinel row missing after seed: {:?}", FILTER, seeded);
    filestore_write(&exec, &pod)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] seed filestore file: {:#}", FILTER, e));
    eprintln!("[e2e-k8s:{}] seeded sentinel row + filestore file", FILTER);

    // ---- backup (mirror `firestream helm backup` CLI arm) ----
    let job_timeout = env_timeout_secs_k8s();
    let backup_job = format!("{}-manual-e2e", target.cronjob);
    rt.block_on(kubectl.create_job_from_cronjob(ns, &target.cronjob, &backup_job))
        .unwrap_or_else(|e| {
            panic!(
                "[e2e-k8s:{}] create backup job from cronjob `{}` (is backup.enabled honored?): {}",
                FILTER, target.cronjob, e
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
            panic!("[e2e-k8s:{}] no `backup complete: <key>` line in backup job logs:\n{}", FILTER, logs)
        });
    eprintln!("[e2e-k8s:{}] backup produced object: {}", FILTER, full_key);

    aws_s3_ls(&exec, &pod, &full_key)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] object not found in object store: {:#}", FILTER, e));

    // ---- simulate data loss ----
    psql(&exec, &pod, DROP_SQL).unwrap_or_else(|e| panic!("[e2e-k8s:{}] drop table: {:#}", FILTER, e));
    filestore_remove(&exec, &pod)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] remove filestore file: {:#}", FILTER, e));
    eprintln!("[e2e-k8s:{}] dropped sentinel table + deleted filestore file", FILTER);

    // ---- restore (the exact CLI code path, including the quiesce) ----
    let from = full_key
        .strip_prefix("s3://firestream/")
        .unwrap_or(&full_key)
        .to_string();
    let restore_job = rt
        .block_on(restore_from_backup(&kubectl, &target, &from, &restore_command(CHART_ODOO), job_timeout))
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] restore failed: {}", FILTER, e));
    eprintln!("[e2e-k8s:{}] restore job `{}` completed; deployment scaled back", FILTER, restore_job);

    // ---- assert on the fresh pod ----
    let deadline = Instant::now() + Duration::from_secs(job_timeout);
    readiness::wait_pods_ready(&handle.kubeconfig, &odoo.release, ns, deadline)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] odoo pods not ready after restore: {:#}", FILTER, e));
    let pod = resolve_pod(&handle.kubeconfig, &selector, ns)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] resolve odoo pod after restore: {:#}", FILTER, e));
    let restored = psql(&exec, &pod, SELECT_SQL)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] read-back after restore: {:#}", FILTER, e));
    assert!(restored.contains(SENTINEL), "[e2e-k8s:{}] sentinel row NOT restored: {:?}", FILTER, restored);
    let file = filestore_read(&exec, &pod)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] filestore file NOT restored: {:#}", FILTER, e));
    assert!(file.contains(SENTINEL), "[e2e-k8s:{}] filestore file content wrong: {:?}", FILTER, file);

    eprintln!("[e2e-k8s:{}] round-trip OK: seed -> backup -> delete -> restore -> assert", FILTER);
}

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

/// First pod name matching `selector`.
fn resolve_pod(kubeconfig: &Path, selector: &str, namespace: &str) -> Result<String> {
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args([
            "get", "pods", "-l", selector, "-n", namespace, "-o",
            "jsonpath={.items[0].metadata.name}",
        ])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get pods")?;
    if !out.status.success() {
        bail!(
            "kubectl get pods (selector={} ns={}): {}",
            selector,
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

/// Run a bash snippet inside the odoo pod with the image's helper library
/// sourced, so `odoo_pg` and the `ODOO_*` connection env are available.
fn odoo_bash(exec: &KubectlExec, pod: &str, script: &str) -> Result<String> {
    let full = format!("source /opt/firestream/scripts/libhelpersodoo.sh && {}", script);
    let out = exec
        .exec(pod, &["bash", "-c", &full])
        .context("kubectl exec bash")?;
    if !out.status.success() {
        bail!(
            "exit {}: stderr {} stdout {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn psql(exec: &KubectlExec, pod: &str, sql: &str) -> Result<String> {
    let script = format!(
        "odoo_pg psql --no-password -v ON_ERROR_STOP=1 -tAc {}",
        shell_quote(sql)
    );
    odoo_bash(exec, pod, &script)
}

fn filestore_write(exec: &KubectlExec, pod: &str) -> Result<String> {
    let script = format!(
        "d=\"$ODOO_DATA_DIR/filestore/$ODOO_DATABASE_NAME\" && mkdir -p \"$d\" && printf '%s\\n' {} > \"$d/{}\"",
        shell_quote(SENTINEL),
        SENTINEL_FILE
    );
    odoo_bash(exec, pod, &script)
}

fn filestore_remove(exec: &KubectlExec, pod: &str) -> Result<String> {
    let script = format!(
        "rm -f \"$ODOO_DATA_DIR/filestore/$ODOO_DATABASE_NAME/{}\"",
        SENTINEL_FILE
    );
    odoo_bash(exec, pod, &script)
}

fn filestore_read(exec: &KubectlExec, pod: &str) -> Result<String> {
    let script = format!(
        "cat \"$ODOO_DATA_DIR/filestore/$ODOO_DATABASE_NAME/{}\"",
        SENTINEL_FILE
    );
    odoo_bash(exec, pod, &script)
}

/// `aws s3 ls <full s3:// key>` inside the odoo pod (awscli2 is baked into
/// the firestream-odoo image). Creds + endpoint are passed explicitly since
/// the SeaweedFS Secret lives cross-namespace.
fn aws_s3_ls(exec: &KubectlExec, pod: &str, full_key: &str) -> Result<()> {
    let out = exec
        .exec(
            pod,
            &[
                "env",
                "HOME=/tmp",
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

/// Single-quote a string for bash.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert!(shell_quote(SEED_SQL).contains("'firestream-sentinel'"));
    }
}
