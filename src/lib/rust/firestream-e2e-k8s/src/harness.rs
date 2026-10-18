//! Harness for the k8s e2e tests.
//!
//! Phase 3 wires the full pipeline: env-gate → mutex → cluster create →
//! preload images → helm deploy → wait pods ready → run probe chain →
//! teardown. The Phase-2 cluster-only sentinel is gone; the cluster +
//! release guards take care of teardown on scope exit (or skip it when
//! `FIRESTREAM_E2E_K8S_KEEP=1`).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use firestream_e2e_core::env::{docker_daemon_check, should_skip};
use firestream_e2e_core::guard::wait_with_budget;
use firestream_e2e_core::k8s::{cluster, probes, readiness};
use firestream_e2e_core::retry::retry_until_sync;
use tracing::warn;

use crate::deploy::{self, DeployedRelease};
use crate::env::{env_charts_dir, env_keep_k8s, env_strict_k8s, env_timeout_secs_k8s, selected_k8s};

/// Required external binaries for the k8s harness. Missing any one
/// short-circuits the test with a "skip" message (or a panic when
/// `FIRESTREAM_E2E_K8S_STRICT=1`).
///
/// `nix` is included because the preload step shells out to
/// `nix run .#<image>-image -- --load` for non-Bitnami images. Phase 3's
/// postgres/redis don't trigger the load path (their `images` map is
/// empty), but Phase 4's airflow does — keep nix in the gate so the
/// matrix tests behave consistently.
const REQUIRED_BINS: &[&str] = &["k3d", "kubectl", "helm", "nix", "docker"];

/// Returns `Some(reason)` if any prerequisite is missing — either a
/// required binary isn't on `PATH`, or the docker daemon is
/// unreachable. Returns `None` when everything's good to go.
pub fn should_skip_k8s() -> Option<String> {
    if let Some(why) = should_skip(REQUIRED_BINS) {
        return Some(why);
    }
    if let Some(why) = docker_daemon_check() {
        return Some(why);
    }
    None
}

/// Cross-test mutex: only one k8s harness test runs at a time within a
/// single process. The 6-char random cluster suffix in
/// [`cluster::create_cluster`] handles the cross-process case; together
/// they serialise cluster ops on any given machine.
pub(crate) fn harness_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII teardown for a deployed helm release. On Drop:
///
/// - When `keep` is true: log + return (operator wants the release left
///   running for inspection).
/// - Otherwise: best-effort `helm uninstall --wait=false` then
///   `kubectl delete namespace --wait=false`, both bounded under a
///   shared 60s budget. Errors are swallowed — by the time this drop
///   runs the test result is already decided; failing teardown should
///   not panic during unwind.
pub struct ReleaseGuard {
    release: DeployedRelease,
    kubeconfig: std::path::PathBuf,
    keep: bool,
}

impl ReleaseGuard {
    pub fn arm(release: DeployedRelease, kubeconfig: std::path::PathBuf, keep: bool) -> Self {
        Self {
            release,
            kubeconfig,
            keep,
        }
    }
}

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if self.keep {
            eprintln!(
                "[e2e-k8s:{}] KEEP=1 — leaving helm release `{}` (ns `{}`) up. \
                 Clean up with `helm --kubeconfig {} uninstall {} -n {}` then \
                 `kubectl --kubeconfig {} delete namespace {} --wait=false`.",
                self.release.chart,
                self.release.release,
                self.release.namespace,
                self.kubeconfig.display(),
                self.release.release,
                self.release.namespace,
                self.kubeconfig.display(),
                self.release.namespace
            );
            return;
        }

        let budget = Duration::from_secs(60);
        let start = Instant::now();

        // helm uninstall first, but don't block on it cleanly finishing
        // — pass --wait=false so helm hands back as soon as the release
        // record is removed; the namespace delete below will reap the
        // remaining objects.
        let mut helm_child = match Command::new("helm")
            .arg("--kubeconfig")
            .arg(&self.kubeconfig)
            .args([
                "uninstall",
                &self.release.release,
                "-n",
                &self.release.namespace,
                "--wait=false",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    release = %self.release.release,
                    ns = %self.release.namespace,
                    err = %e,
                    "spawn helm uninstall failed; falling through to namespace delete"
                );
                // No child to wait on; skip to namespace delete.
                let _ = drop_namespace(&self.kubeconfig, &self.release.namespace, budget);
                return;
            }
        };

        match wait_with_budget(&mut helm_child, half(budget)) {
            Ok(()) => {}
            Err(()) => {
                warn!(
                    release = %self.release.release,
                    ns = %self.release.namespace,
                    "helm uninstall exceeded budget; killing"
                );
                let _ = helm_child.kill();
                let _ = helm_child.wait();
            }
        }

        // Whatever time is left in the budget goes to the namespace delete.
        let remaining = budget.saturating_sub(start.elapsed());
        let _ = drop_namespace(&self.kubeconfig, &self.release.namespace, remaining);
    }
}

/// Half the budget, but at least 5s. Avoids handing zero-second
/// budgets to wait_with_budget on a fast machine.
fn half(d: Duration) -> Duration {
    let h = d.as_secs() / 2;
    Duration::from_secs(h.max(5))
}

/// Best-effort `kubectl delete namespace --wait=false` under a budget.
/// Always returns `Ok(())` — the caller (Drop) cannot meaningfully act
/// on a failure.
fn drop_namespace(kubeconfig: &Path, ns: &str, budget: Duration) -> Result<(), ()> {
    if budget.is_zero() {
        // Spawn but don't wait; fire-and-forget.
        let _ = Command::new("kubectl")
            .arg("--kubeconfig")
            .arg(kubeconfig)
            .args(["delete", "namespace", ns, "--wait=false"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        return Ok(());
    }

    let child = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["delete", "namespace", ns, "--wait=false"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    match wait_with_budget(&mut child, budget) {
        Ok(()) => Ok(()),
        Err(()) => {
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        }
    }
}

/// Resolve the bundle directory, materialising it via `nix build` if no
/// path on the resolution list exists on disk. The resolution order is
/// already encoded in `env_charts_dir()`; this function adds the
/// nix-build fallback because the dev shell's `FIRESTREAM_CHARTS_DIR` is
/// only exported inside `nix develop`.
pub(crate) fn resolve_charts_dir(chart: &str) -> std::path::PathBuf {
    let primary = env_charts_dir();
    if primary.join("index.json").exists() {
        return primary;
    }
    eprintln!(
        "[e2e-k8s:{}] charts dir {} has no index.json; trying `nix build .#firestream-charts-bundle --no-link --print-out-paths`",
        chart,
        primary.display()
    );
    let out = Command::new("nix")
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            ".#firestream-charts-bundle",
        ])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !p.is_empty() {
                eprintln!("[e2e-k8s:{}] using nix-built bundle at {}", chart, p);
                return std::path::PathBuf::from(p);
            }
            eprintln!(
                "[e2e-k8s:{}] nix build returned success but no path; falling back to {}",
                chart,
                primary.display()
            );
            primary
        }
        Ok(o) => {
            eprintln!(
                "[e2e-k8s:{}] nix build for charts bundle exited with {}: {}",
                chart,
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            primary
        }
        Err(e) => {
            eprintln!(
                "[e2e-k8s:{}] spawning nix build failed: {}; falling back to {}",
                chart,
                e,
                primary.display()
            );
            primary
        }
    }
}

/// Full Phase-3 pipeline. Steps:
///
///  1. Skip / strict gate.
///  2. Filter gate via `FIRESTREAM_E2E_K8S_STACKS`.
///  3. Take the process-wide mutex (poisoning-tolerant).
///  4. Create a per-test k3d cluster. Arm `ClusterGuard`.
///  5. Resolve the bundle dir; deploy the chart via
///     `deploy_chart_lifecycle`. Arm `ReleaseGuard`.
///  6. `kubectl wait --for=condition=Ready` against the release's pods,
///     bounded by `FIRESTREAM_E2E_K8S_TIMEOUT_SECS`.
///  7. Run the per-chart probe chain in `retry_until_sync` loops against
///     the same deadline.
///
/// Panics on any pipeline failure (matches the docker harness's contract;
/// `#[test]` consumers expect panic-on-failure semantics from
/// `run_one`).
pub fn run_one(chart: &str) {
    // ---- 1. skip gate ----
    if let Some(reason) = should_skip_k8s() {
        if env_strict_k8s() {
            panic!(
                "[e2e-k8s:{}] STRICT=1 and prerequisite missing: {}",
                chart, reason
            );
        }
        eprintln!("[e2e-k8s:{}] SKIP: {}", chart, reason);
        return;
    }

    // ---- 2. filter gate ----
    if !selected_k8s(chart) {
        eprintln!(
            "[e2e-k8s:{}] SKIP: not in FIRESTREAM_E2E_K8S_STACKS",
            chart
        );
        return;
    }

    // ---- 3. process-wide mutex ----
    let _guard = harness_lock().lock().unwrap_or_else(|p| p.into_inner());

    // ---- 4. create cluster + arm guard ----
    let handle = match cluster::create_cluster(chart) {
        Ok(h) => h,
        Err(e) => panic!("[e2e-k8s:{}] cluster create failed: {:#}", chart, e),
    };
    eprintln!(
        "[e2e-k8s:{}] cluster up: name={} kubeconfig={}",
        chart,
        handle.name,
        handle.kubeconfig.display()
    );
    let _cluster_guard = cluster::ClusterGuard::arm(handle.clone(), env_keep_k8s());

    // ---- 5. resolve bundle + deploy + arm release guard ----
    let charts_dir = resolve_charts_dir(chart);
    // On failure, dump cluster state BEFORE unwinding — `_cluster_guard` is
    // already armed, so the panic destroys the whole cluster and with it every
    // artefact that could explain what went wrong. `--atomic` is off for e2e
    // (see deploy.rs) specifically so the workloads are still around here.
    //
    // The namespace is not known until deploy returns Ok, so on the failure
    // path fall back to the chart name — which is what every chart manifest in
    // the bundle uses as its release namespace.
    let release = deploy::deploy_chart(&handle, &charts_dir, chart).unwrap_or_else(|e| {
        crate::diagnostics::dump_namespace(chart, &handle.kubeconfig, chart);
        panic!("[e2e-k8s:{}] deploy failed: {:#}", chart, e)
    });
    eprintln!(
        "[e2e-k8s:{}] deployed: release={} namespace={}",
        chart, release.release, release.namespace
    );
    let _release_guard = ReleaseGuard::arm(
        release.clone(),
        handle.kubeconfig.clone(),
        env_keep_k8s(),
    );

    // ---- 6. wait pods Ready ----
    let deadline = Instant::now() + Duration::from_secs(env_timeout_secs_k8s());
    readiness::wait_pods_ready(
        &handle.kubeconfig,
        &release.release,
        &release.namespace,
        deadline,
    )
    .unwrap_or_else(|e| {
        // Same reasoning as the deploy path: capture before the guards unwind.
        // This is the arm that would have shown superset's OOMKilled worker
        // instead of an opaque `timed out waiting for the condition`.
        crate::diagnostics::dump_namespace(chart, &handle.kubeconfig, &release.namespace);
        panic!("[e2e-k8s:{}] wait_pods_ready: {:#}", chart, e)
    });
    eprintln!("[e2e-k8s:{}] pods Ready", chart);

    // ---- 7. probe chain ----
    let ctx = probes::K8sCtx {
        handle: handle.clone(),
        namespace: release.namespace.clone(),
        release: release.release.clone(),
        deadline,
    };
    let chain = probes::for_chart(chart, &ctx)
        .unwrap_or_else(|e| panic!("[e2e-k8s:{}] probe chain: {:#}", chart, e));
    for probe in chain {
        eprintln!("[e2e-k8s:{}] probe: {}", chart, probe.name());
        retry_until_sync(&deadline, || probe.run(&ctx))
            .unwrap_or_else(|e| panic!("[e2e-k8s:{}] probe {} failed: {:#}", chart, probe.name(), e));
    }

    eprintln!("[e2e-k8s:{}] all probes passed", chart);
    // _release_guard drops first (helm uninstall + ns delete), then
    // _cluster_guard (k3d cluster delete + registry delete).
}
