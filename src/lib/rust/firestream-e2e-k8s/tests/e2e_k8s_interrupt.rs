//! SIGTERM during `k3d cluster create` leaves no object of the run.
//!
//! ```text
//! make test-e2e-cleanup
//! cargo test -p firestream-e2e-k8s --test e2e_k8s_interrupt -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! This file is its own test binary, so `make test-e2e-k8s` does not run it.

use firestream_e2e_core::interrupt;
use firestream_e2e_k8s::harness::should_skip_k8s;

const TARGET: &str = "interrupt";

/// The child: the setup half of `e2e_k8s_cluster_lifecycle`. The parent
/// kills it while the cluster is being created.
#[test]
#[ignore = "e2e-k8s: child of e2e_k8s_sigterm_during_create; does nothing on its own"]
fn e2e_k8s_interrupt_child() {
    if !interrupt::is_child() {
        return;
    }
    use firestream_e2e_core::k8s::cluster;

    firestream_e2e_core::cleanup::install_signal_handler().expect("install signal handler");
    let _host_lock = firestream_e2e_core::lock::HostLock::acquire().expect("acquire host e2e lock");
    let handle = cluster::create_cluster(TARGET).expect("create_cluster");
    let _guard = cluster::ClusterGuard::arm(handle, false);
    // NOTE: a parent that signals late still lands here; the sleep keeps the
    // guard armed so the signal drain, not the guard's Drop, tears down.
    std::thread::sleep(std::time::Duration::from_secs(600));
}

#[test]
#[ignore = "e2e-k8s: needs k3d+docker; run via `make test-e2e-cleanup`"]
fn e2e_k8s_sigterm_during_create() {
    if let Some(reason) = should_skip_k8s() {
        eprintln!("[e2e-k8s:interrupt] SKIP: {reason}");
        return;
    }
    let report = interrupt::run("e2e_k8s_interrupt_child", TARGET, &[]).expect("interrupt run");
    eprintln!(
        "[e2e-k8s:interrupt] run {} signalled after {:?}, exit {}",
        report.run, report.signalled_after, report.status
    );
    report.assert_clean();
}
