//! Transport-agnostic primitives shared by every Firestream end-to-end harness.
//!
//! Phase 1 of the k3d/helm e2e plan extracted these symbols from
//! `src/lib/rust/firestream/tests/e2e/` so a second harness (the k8s one in
//! Phase 2+) can sit on the same foundation as the existing docker-compose
//! harness. Nothing in this crate knows about docker, k8s, helm, or nix —
//! callers wire their own backends via the [`exec::Exec`] trait and pass
//! backend-specific data (compose file path / cluster name / kubeconfig) only
//! through the harness layer above.
//!
//! # Modules
//!
//! - [`probe`] — `Probe` trait + base impls (`Tcp`, `HttpReady`,
//!   `PgIsReady`, `RedisPing`, `KafkaApiVersions`). The last three are
//!   generic over an `Exec` impl so the same code drives `docker compose
//!   exec` and `kubectl exec`.
//! - [`retry`] — `retry_until_sync` deadline loop used to drive a probe
//!   until success-or-deadline.
//! - [`env`] — accessors for the `FIRESTREAM_E2E_*` env-var contract:
//!   `env_strict`, `env_keep`, `env_timeout_secs`, plus a generic
//!   `selected()` filter gate and a generic `should_skip()` PATH check.
//! - [`guard`] — `wait_with_budget`, the bounded-wait helper used by
//!   teardown guards' `Drop` implementations.
//! - [`exec`] — the transport-agnostic `Exec` trait. One impl per backend
//!   (`DockerComposeExec` lives in the docker harness; `KubectlExec` lives
//!   in [`k8s::exec`]).
//! - [`run_id`] — one run id per process and the
//!   `firestream-e2e-<target>-<id>` resource names built from it.
//! - [`lock`] — the host-wide `flock` that every harness test and every
//!   sweep holds.
//! - [`cleanup`] — process-wide teardown registry, drained on
//!   SIGINT/SIGTERM/SIGHUP.
//! - [`runner`] — the `CommandRunner` seam for docker/k3d/kubectl calls.
//! - [`sweep`] — find and remove harness-owned leftovers (library side of
//!   the `firestream-e2e-sweep` binary).
//! - [`leak`] — post-teardown leak check for one run.
//! - [`interrupt`] — SIGTERM a child harness run during setup and prove
//!   that it leaves nothing.
//! - [`k8s`] — k3d cluster lifecycle, `kubectl`-driven probe matrix,
//!   port-forward and readiness helpers. Relocated here from
//!   `firestream-e2e-k8s` so the main `firestream` CLI can reuse it
//!   without forming a dependency cycle.

pub mod cleanup;
pub mod env;
pub mod exec;
pub mod guard;
pub mod interrupt;
pub mod k8s;
pub mod leak;
pub mod lock;
pub mod probe;
pub mod retry;
pub mod run_id;
pub mod runner;
pub mod supabase;
pub mod sweep;

#[cfg(test)]
mod test_child;
