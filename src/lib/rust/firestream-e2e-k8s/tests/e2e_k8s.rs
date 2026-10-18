//! Phase 3: cluster_lifecycle + postgresql + redis. Phase 4 fills in
//! the remaining 6 charts (airflow, kafka, spark, jupyterhub,
//! superset, odoo).
//!
//! Run individually:
//!
//! ```text
//! # Cluster lifecycle smoke (no chart deploy — cheaper than a full e2e):
//! cargo test -p firestream-e2e-k8s --test e2e_k8s -- \
//!     --ignored --nocapture e2e_k8s_cluster_lifecycle
//!
//! # Full chart e2e (deploy + wait + probe + teardown):
//! nix shell nixpkgs#k3d --command \
//!     cargo test -p firestream-e2e-k8s --test e2e_k8s -- \
//!         --ignored --test-threads=1 --nocapture e2e_k8s_postgresql
//! ```
//!
//! `k3d` is not on the devshell PATH yet (Phase-2 hand-off note); the
//! `nix shell nixpkgs#k3d --command …` wrap is a temporary workaround
//! until Phase 4 adds `k3d` to the devshell.

/// One `#[test] #[ignore]` per chart, all routed through
/// `harness::run_one`. Phase 4 will append more `stack_test_k8s!`
/// invocations as the probe chains for the remaining charts come
/// online.
macro_rules! stack_test_k8s {
    ($fn_name:ident, $chart:literal) => {
        #[test]
        #[ignore = "e2e-k8s: needs k3d+kubectl+helm+nix+docker; run via `nix shell nixpkgs#k3d --command cargo test -p firestream-e2e-k8s --test e2e_k8s -- --ignored --test-threads=1 --nocapture`"]
        fn $fn_name() {
            firestream_e2e_k8s::harness::run_one($chart);
        }
    };
}

/// Phase-2 carry-over: a no-deploy smoke that just creates and tears
/// down a cluster. Useful as a fast sanity check (the chart deploys
/// take 1-5 minutes each on a warm cache).
#[test]
#[ignore = "e2e-k8s: cluster smoke; cheaper than a full chart deploy"]
fn e2e_k8s_cluster_lifecycle() {
    // Bypass harness::run_one — we want to assert on the cluster
    // directly. The chart tests below go through run_one.
    use firestream_e2e_core::k8s::cluster;
    use firestream_e2e_k8s::{env::env_keep_k8s, harness::should_skip_k8s};

    if let Some(reason) = should_skip_k8s() {
        eprintln!("[e2e-k8s:lifecycle] SKIP: {}", reason);
        return;
    }

    let handle = cluster::create_cluster("lifecycle").expect("create_cluster");
    eprintln!(
        "[e2e-k8s:lifecycle] cluster up: name={} kubeconfig={}",
        handle.name,
        handle.kubeconfig.display()
    );
    let _guard = cluster::ClusterGuard::arm(handle.clone(), env_keep_k8s());

    // Assert kubectl --kubeconfig <handle.kubeconfig> get nodes returns
    // at least one node. We don't bother asserting Ready here — k3d's
    // own setup_cluster path (which we don't take) is what blocks on
    // CoreDNS/etc.; for the lifecycle sentinel just proving the API
    // is reachable is the point.
    let output = std::process::Command::new("kubectl")
        .args(["--kubeconfig"])
        .arg(&handle.kubeconfig)
        .args(["get", "nodes", "-o", "name"])
        .output()
        .expect("kubectl get nodes");
    assert!(
        output.status.success(),
        "kubectl get nodes failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nodes = String::from_utf8_lossy(&output.stdout);
    assert!(
        nodes.lines().count() >= 1,
        "expected >=1 node, got: {:?}",
        nodes
    );
    eprintln!("[e2e-k8s:lifecycle] nodes={}", nodes.lines().count());
}

// ---- Phase 3 charts ----
stack_test_k8s!(e2e_k8s_postgresql, "postgresql");
stack_test_k8s!(e2e_k8s_redis, "redis");

// ---- Phase 4 charts ----
stack_test_k8s!(e2e_k8s_kafka, "kafka");
stack_test_k8s!(e2e_k8s_airflow, "airflow");
stack_test_k8s!(e2e_k8s_spark, "spark");
stack_test_k8s!(e2e_k8s_jupyterhub, "jupyterhub");
stack_test_k8s!(e2e_k8s_superset, "superset");
stack_test_k8s!(e2e_k8s_odoo, "odoo");

// ---- PostgreSQL backup/restore round-trip (multi-chart) ----
// Deploys seaweedfs + postgresql (backup.enabled=true), seeds a sentinel
// row, runs an on-demand backup (the same KubectlClient path the
// `firestream helm backup` CLI uses), asserts the object lands in
// SeaweedFS, drops the table, restores from the produced key, and asserts
// the row is back. Gated by the `pg-backup` filter token:
//   FIRESTREAM_E2E_K8S_STACKS=pg-backup
#[test]
#[ignore = "e2e-k8s: pg backup/restore round-trip; needs k3d+kubectl+helm+nix+docker; run via `make test-e2e-k8s-pg-backup`"]
fn e2e_k8s_pg_backup() {
    firestream_e2e_k8s::pg_backup::run_pg_backup_roundtrip();
}

// ---- Odoo backup/restore round-trip (multi-chart) ----
// Deploys seaweedfs + odoo (backup.enabled=true), seeds a sentinel row in
// the Odoo database and a sentinel file in the filestore, runs an
// on-demand backup, deletes both, restores through the SAME
// `restore_from_backup` path the `firestream helm restore` CLI uses (which
// scales the Odoo Deployment to zero and back), and asserts both are back.
// Gated by the `odoo-backup` filter token:
//   FIRESTREAM_E2E_K8S_STACKS=odoo-backup
#[test]
#[ignore = "e2e-k8s: odoo backup/restore round-trip; needs k3d+kubectl+helm+nix+docker; run via `make test-e2e-k8s-odoo-backup`"]
fn e2e_k8s_odoo_backup() {
    firestream_e2e_k8s::odoo_backup::run_odoo_backup_roundtrip();
}

// ---- Object store (non-Bitnami chart) ----
// SeaweedFS is the default local S3 object store. Its all-in-one pod is
// deployed first in the dev stack (object store up before consumers).
// The harness probe GETs the S3 gateway on :8333 (see
// firestream_e2e_core::k8s::probes `for_chart` "seaweedfs" arm).
stack_test_k8s!(e2e_k8s_seaweedfs, "seaweedfs");

// ---- Edge proxy (non-Bitnami chart) ----
// The per-namespace reverse proxy. Deployed with its default `upstreams: {}`,
// so it fronts nothing and answers its own 404 — which is the point: the probe
// hits `/healthz`, the chart's local non-proxying health location, proving the
// proxy comes up and stays Ready independently of any backend.
//
// Its sibling `cloudflared` has NO test here on purpose: the connector reports
// Ready only once it has registered with the Cloudflare edge, so it cannot come
// up in a hermetic cluster at all.
stack_test_k8s!(e2e_k8s_nginx, "nginx");
