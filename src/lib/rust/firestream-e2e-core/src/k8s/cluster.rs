//! Per-test k3d cluster lifecycle.
//!
//! [`create_cluster`] stands up a fresh, minimum-footprint k3d cluster
//! named `firestream-e2e-<chart>-<run id>` (see [`crate::run_id`]; 1
//! server, 0 agents, built-in registry, no LB ports). The run id is
//! shared by every resource this process creates, so the cross-process
//! collision case is "another run id", and the in-process case (the
//! same `chart` requested twice) is caught explicitly — see
//! [`reserve_name`].
//!
//! [`ClusterGuard`] tears the cluster down on Drop under a 60s budget,
//! by running the same [`crate::sweep::plan`]/[`crate::sweep::execute`]
//! a sweep uses for one named resource. Honors `FIRESTREAM_E2E_K8S_KEEP=1`
//! (logs and returns instead of deleting) so operators can inspect a
//! failing cluster post-test.

use std::collections::HashSet;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use k8s_manager::{
    K3dClusterConfig, K3dClusterManager, K3dDevModeConfig, K3dNetworkConfig, K3dRegistryConfig,
    K3dTimeoutConfig, K3dTlsConfig,
};
use tracing::warn;

use crate::cleanup::{self, Registration};
use crate::lock::HostLock;
use crate::run_id::RunId;
use crate::runner::SystemRunner;
use crate::sweep::{self, Scope};

/// Handle to a live per-test k3d cluster. Cheap to `Clone` so the
/// guard can own one while the test still holds another for kubectl
/// access.
#[derive(Debug, Clone)]
pub struct ClusterHandle {
    /// Cluster name as known to `k3d` (without the `k3d-` prefix that
    /// k3d uses for container/network names).
    pub name: String,
    /// Path to a kubeconfig pinned to this cluster only. Hand this to
    /// `KubectlExec` or pass via `kubectl --kubeconfig`.
    pub kubeconfig: PathBuf,
    /// Registry container name (matches what `k3d` emits for the
    /// built-in registry).
    pub registry: String,
}

/// Process-wide set of cluster names this process has already issued.
fn issued_names() -> &'static Mutex<HashSet<String>> {
    static ISSUED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ISSUED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Reserve `firestream-e2e-<chart>-<run id>` for this process. Fails
/// clearly if `chart` was already reserved — a second `create_cluster`
/// call for the same target is a harness bug (it would otherwise
/// collide on the same k3d cluster name), not a retry worth silently
/// allowing.
/// k3d `cluster create` refuses a longer name.
const K3D_MAX_CLUSTER_NAME: usize = 32;

fn reserve_name(chart: &str) -> Result<String> {
    let name = RunId::current().resource_name(chart);
    if name.len() > K3D_MAX_CLUSTER_NAME {
        bail!(
            "e2e cluster name {name} has {} characters; k3d allows {K3D_MAX_CLUSTER_NAME}. \
             Use a target of at most {} characters for create_cluster({chart:?})",
            name.len(),
            chart.len() - (name.len() - K3D_MAX_CLUSTER_NAME)
        );
    }
    let mut issued = issued_names().lock().unwrap_or_else(|p| p.into_inner());
    if !issued.insert(name.clone()) {
        bail!(
            "e2e cluster name {name} was already issued by this process; \
             create_cluster({chart:?}) was called twice for the same target"
        );
    }
    Ok(name)
}

/// Reserve an ephemeral TCP port by binding `127.0.0.1:0`, reading the
/// kernel-assigned port, and releasing the listener. There's a small
/// race window between release and the consumer (k3d / k8s-manager's
/// registry health-check) binding, but for the lifetime of a single
/// per-test cluster it's good enough — collisions would manifest as a
/// cluster-create failure, which the test harness reports clearly.
///
/// We need this because `K3dClusterConfig`'s api_port / http_port /
/// https_port / registry.port are `u16` (not `Option<u16>`) and
/// `setup_registry` does `localhost:<port>/v2/` health-checks — so
/// `0` cannot be passed through as "let k3d auto-pick".
fn pick_ephemeral_port() -> Result<u16> {
    let listener =
        TcpListener::bind("127.0.0.1:0").context("bind 127.0.0.1:0 to pick ephemeral port")?;
    let port = listener
        .local_addr()
        .context("read local_addr for ephemeral port")?
        .port();
    drop(listener);
    Ok(port)
}

/// Build the minimum-footprint cluster config for an e2e test. Port
/// numbers are reserved via `pick_ephemeral_port` rather than fixed
/// constants so parallel runs (across processes) don't trip over each
/// other on host ports.
fn build_config(name: &str) -> Result<K3dClusterConfig> {
    // We avoid touching the host's /etc/hosts and routing tables —
    // e2e tests must be a no-op on the developer's machine outside the
    // cluster they create.
    let network = K3dNetworkConfig {
        configure_routes: false,
        configure_dns: false,
        patch_etc_hosts: false,
        // Defaults are fine for pod/service CIDRs; we never expose them.
        pod_cidr: "10.42.0.0/16".to_string(),
        service_cidr: "10.43.0.0/16".to_string(),
        // Bind only to 127.0.0.1 — the API server doesn't need to be
        // reachable from anything except local kubectl. This also
        // means concurrent clusters on the same host don't collide on
        // 0.0.0.0:<port> the way the default config would.
        api_bind_address: "127.0.0.1".to_string(),
        disable_iptables_routing: true,
    };

    // Reserve ephemeral ports up front. `K3dClusterConfig`'s port
    // fields are `u16` (not Option), and the underlying create_cluster
    // path always emits `-p <port>:<port>@loadbalancer` for both
    // http/https plus a registry health-check on localhost:<port>/v2/
    // — so we cannot pass 0 anywhere here, even though we'd love k3d
    // to auto-pick. Picking these here at least keeps the harness
    // from hardcoding well-known host ports.
    let api_port = pick_ephemeral_port()?;
    let http_port = pick_ephemeral_port()?;
    let https_port = pick_ephemeral_port()?;
    let registry_port = pick_ephemeral_port()?;

    Ok(K3dClusterConfig {
        name: name.to_string(),
        api_port,
        http_port,
        https_port,
        servers: 1,
        agents: 0,
        k3s_version: "v1.31.2-k3s1".to_string(),
        registry: K3dRegistryConfig {
            enabled: true,
            // Cluster-scoped registry name so `delete_cluster` can
            // also drop the registry (see manager.rs:466-475 in
            // k8s-manager — if the name contains the cluster name,
            // the registry is treated as cluster-scoped).
            name: format!("{}-registry", name),
            port: registry_port,
        },
        tls: K3dTlsConfig {
            // We don't need TLS for e2e probes; certificates are a
            // multi-second cost we can skip.
            enabled: false,
            ..K3dTlsConfig::default()
        },
        network,
        dev_mode: None::<K3dDevModeConfig>,
        timeouts: K3dTimeoutConfig::default(),
        // The e2e harness reads this cluster's kubeconfig straight from
        // `k3d kubeconfig get` (see `write_kubeconfig`) and must never
        // touch `$KUBECONFIG` / `~/.kube/config` — see the field doc on
        // `K3dClusterConfig::isolate_kubeconfig`.
        isolate_kubeconfig: true,
    })
}

/// Create a fresh per-test k3d cluster scoped to `chart`. The returned
/// [`ClusterHandle`] owns no resources directly — wrap it in a
/// [`ClusterGuard`] for RAII teardown.
///
/// The cluster name is `firestream-e2e-<chart>-<run id>` (see
/// [`reserve_name`]) — the exact string passed to `k3d cluster create`,
/// so it's also what `k3d cluster list` and
/// `docker ps --filter "name=k3d-<…>"` show after creation.
///
/// A teardown for `name` is registered with [`crate::cleanup`] before
/// the cluster (and its registry) exist, so a signal or a failure
/// partway through create tears down whatever got made. On success the
/// registration is hand off: this function disarms its own (without
/// running it) and [`ClusterGuard::arm`] takes out a fresh one scoped to
/// the handle's full lifetime.
///
/// `K3dClusterManager::create_cluster` is async; we wrap it in a
/// current-thread tokio runtime so this crate's public API stays
/// synchronous (matching Phase 1's convention).
pub fn create_cluster(chart: &str) -> Result<ClusterHandle> {
    let name = reserve_name(chart)?;
    let config = build_config(&name)?;
    let registry_name = format!("k3d-{}-registry", &name);
    let manager = K3dClusterManager::new(config);

    cleanup::install_signal_handler().context("install e2e signal handler")?;
    let registration = cleanup::register(format!("k3d cluster {name}"), {
        let name = name.clone();
        move || teardown(&name)
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio current-thread runtime for cluster create")?;

    if let Err(e) = rt.block_on(manager.create_cluster()) {
        registration.run();
        return Err(e).with_context(|| format!("k3d cluster create {}", name));
    }

    // k3d-the-binary writes a per-cluster kubeconfig we can fetch with
    // `k3d kubeconfig get <cluster>`. Stash it under temp_dir so it
    // doesn't leak into the user's $HOME/.kube/config; we never call
    // `merge` so we never touch any global config.
    let kubeconfig_path = std::env::temp_dir().join(format!("{}.kubeconfig", name));
    if let Err(e) = write_kubeconfig(&name, &kubeconfig_path) {
        registration.run();
        return Err(e).with_context(|| {
            format!(
                "writing kubeconfig for {} to {}",
                name,
                kubeconfig_path.display()
            )
        });
    }

    // The cluster is fully up; `ClusterGuard::arm` takes out its own
    // registration for the handle's full lifetime.
    registration.disarm();

    Ok(ClusterHandle {
        name,
        kubeconfig: kubeconfig_path,
        registry: registry_name,
    })
}

/// Run `k3d kubeconfig get <name>` and persist stdout to `path`. We
/// can't use `K3dClusterManager::reconnect_cluster` because that
/// writes to the default location and mutates `$HOME/.kube/config`.
fn write_kubeconfig(cluster: &str, path: &std::path::Path) -> Result<()> {
    let out = Command::new("k3d")
        .args(["kubeconfig", "get", cluster])
        .stdin(Stdio::null())
        .output()
        .context("spawn k3d kubeconfig get")?;
    if !out.status.success() {
        bail!(
            "k3d kubeconfig get {} failed: status={} stderr={}",
            cluster,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    fs::write(path, &out.stdout)
        .with_context(|| format!("write kubeconfig to {}", path.display()))?;
    Ok(())
}

/// Remove every object named `name`: registry, cluster, leftover `app=k3d`
/// containers, network, `-images` volume, kubeconfig file, kubeconfig
/// context/cluster/user — in that order ([`sweep::plan`]'s order for a
/// single [`Scope::Name`]). Used both by [`ClusterGuard`]'s normal Drop
/// and, via [`crate::cleanup`], by a signal arriving mid-create or
/// mid-test.
fn teardown(name: &str) {
    let lock = match HostLock::acquire() {
        Ok(l) => l,
        Err(e) => {
            warn!(cluster = %name, err = %e, "could not take the host e2e lock for teardown");
            return;
        }
    };
    let scope = Scope::Name(name.to_string());
    let inv = sweep::discover(&SystemRunner, &scope, &std::env::temp_dir());
    let actions = sweep::plan(&inv);
    let report = sweep::execute(&SystemRunner, &actions, &lock);
    for (action, outcome) in report.failures() {
        warn!(cluster = %name, %action, ?outcome, "e2e cluster teardown step failed");
    }
}

/// RAII guard that tears `handle`'s cluster down on Drop (registry,
/// cluster, network, volume, kubeconfig — see [`teardown`]), under a
/// 60s budget per `k3d`/`docker` call (see [`crate::guard::TEARDOWN_BUDGET`]
/// via [`crate::runner::SystemRunner`]).
///
/// Skipped entirely when `keep` is true (`FIRESTREAM_E2E_K8S_KEEP=1`), so
/// an operator can inspect a failing cluster post-test. The drop path is
/// panic-safe: nothing inside it unwraps a poisoned lock or `.expect()`s
/// an env var.
pub struct ClusterGuard {
    handle: ClusterHandle,
    keep: bool,
    registration: Option<Registration>,
}

impl ClusterGuard {
    /// Construct a new guard. Use `keep = env_keep_k8s()` from the
    /// harness to honor the contract env var.
    pub fn arm(handle: ClusterHandle, keep: bool) -> Self {
        let registration = if keep {
            None
        } else {
            let name = handle.name.clone();
            Some(cleanup::register(format!("k3d cluster {name}"), move || {
                teardown(&name)
            }))
        };
        Self {
            handle,
            keep,
            registration,
        }
    }

    /// Borrow the underlying handle (for tests that want to inspect
    /// the cluster without consuming the guard).
    pub fn handle(&self) -> &ClusterHandle {
        &self.handle
    }
}

impl Drop for ClusterGuard {
    fn drop(&mut self) {
        if self.keep {
            eprintln!(
                "[e2e-k8s:{}] KEEP=1 — leaving cluster up. \
                 Clean up manually with `k3d cluster delete {}` (and \
                 `k3d registry delete k3d-{}-registry`).",
                self.handle.name, self.handle.name, self.handle.name
            );
            return;
        }
        if let Some(r) = self.registration.take() {
            r.run();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_name_fails_on_reuse() {
        reserve_name("rsv-once").expect("first reservation ok");
        let err = reserve_name("rsv-once").unwrap_err();
        assert!(err.to_string().contains("already issued"), "{err}");
    }

    #[test]
    fn reserve_name_fails_past_the_k3d_limit() {
        reserve_name("ten-chars0").expect("a 10-character target fits");
        let err = reserve_name("eleven-char").unwrap_err();
        assert!(err.to_string().contains("at most 10 characters"), "{err}");
    }

    /// `arm(keep = true)` must not register a teardown (so a signal or a
    /// later drop never deletes a cluster the caller asked to keep);
    /// `arm(keep = false)` must register one, and dropping the guard must
    /// remove it again (whether or not the named cluster actually exists).
    ///
    /// WARNING: takes `cleanup::test_serial()` — `cleanup::{register,
    /// pending}` touch one process-wide registry, and cargo runs tests in
    /// parallel threads of one process. Without this lock, `cleanup`'s own
    /// `drain` tests race this test's entries.
    #[test]
    fn guard_registers_teardown_only_without_keep() {
        let _serial = cleanup::test_serial().lock().unwrap_or_else(|p| p.into_inner());
        let handle = ClusterHandle {
            name: "firestream-e2e-cluster-test-guard-marker".to_string(),
            kubeconfig: std::env::temp_dir().join("firestream-e2e-cluster-test-guard-marker.kubeconfig"),
            registry: "k3d-firestream-e2e-cluster-test-guard-marker-registry".to_string(),
        };

        let keep_guard = ClusterGuard::arm(handle.clone(), true);
        assert!(!cleanup::pending().iter().any(|l| l.contains(&handle.name)));
        drop(keep_guard);

        let guard = ClusterGuard::arm(handle.clone(), false);
        assert!(cleanup::pending().iter().any(|l| l.contains(&handle.name)));
        drop(guard);
        assert!(!cleanup::pending().iter().any(|l| l.contains(&handle.name)));
    }
}
