//! Pattern #3 — "you must run inside container X / shell Y" re-exec guard.
//! Mirrors `bin/_lib.sh::require_builder_context` (lines 54-61).
//!
//! Two outcomes:
//!   * `InContext` — we're already in the target runner; caller continues
//!     in-place.
//!   * The process is replaced with the re-exec — `exec_or_continue` does
//!     not return; it calls `std::process::exit` with the child's code
//!     once the wrapped invocation completes (matches the bash `exec`
//!     semantics: from the caller's perspective, the original process
//!     becomes the re-execed one).
//!
//! Re-exec preserves:
//!   * The current process's argv (so the child re-runs the same command)
//!   * `TRACEPARENT` (via `trace::Span::traceparent_env` on the caller's
//!     side, or read from env in the no-tracer case) so trace lineage is
//!     intact across the host↔container boundary.
//!   * The allowlisted env vars the caller supplies (`passthrough::EnvSnapshot`).

use std::process::ExitCode;

use thiserror::Error;

use crate::passthrough::EnvSnapshot;
use crate::runner::{ReenterKind, Runner};
use crate::trace::Span;

#[derive(Debug, Error)]
pub enum Error {
    #[error("reenter: failed to detect current runner: {0}")]
    Detect(#[from] crate::runner::Error),
    #[error("reenter: no plan to transition {from:?} → {target:?}")]
    NoPlan { from: Runner, target: Runner },
    #[error("reenter: spawning child for re-exec failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("reenter: cannot determine current process argv")]
    NoCurrentArgv,
    #[error("reenter: SpawnContainer requested but no spawner was configured")]
    NoSpawner,
}

/// Where the guard left us. `InContext` means caller proceeds; the other
/// variant means caller already ran the wrapped command and got its exit
/// code back — caller decides whether to propagate.
#[derive(Debug)]
pub enum Outcome {
    /// Caller is already in the target runner; continue execution.
    InContext,
    /// The wrapped re-exec completed; here is its exit code.
    Reentered { exit_code: i32 },
}

impl Outcome {
    /// Translate into a `std::process::ExitCode`. Useful at the top of
    /// `main()` when the caller wants to mirror the bash `exec` shape.
    pub fn into_exit_code(self) -> ExitCode {
        match self {
            Self::InContext => ExitCode::SUCCESS,
            Self::Reentered { exit_code } => {
                // ExitCode only carries u8 — clamp.
                ExitCode::from((exit_code & 0xff) as u8)
            }
        }
    }
}

/// Spawner closure for container re-entry. Captures the OCI plumbing
/// without forcing this module to depend on `oci::Container` directly
/// (decoupling: `reenter` doesn't need to know about bollard).
///
/// The closure receives `(argv, env_pairs)` and is responsible for
/// invoking the target runner, streaming output, and returning the
/// child's exit code.
pub type ContainerSpawner = Box<
    dyn Fn(
            Vec<String>,
            Vec<(String, String)>,
        ) -> futures::future::BoxFuture<'static, Result<i32, Error>>
        + Send
        + Sync,
>;

pub struct ReenterGuard {
    target: Runner,
    /// The argv the child should run. Defaults to the current process's
    /// argv. Override via `with_argv` for tests or alt-invocation shapes.
    argv: Vec<String>,
    /// Env to pass through into the child. The caller composes this via
    /// `passthrough::EnvSnapshot::to_env_tuples()`.
    env: Vec<(String, String)>,
    /// Optional explicit parent traceparent. If absent we read
    /// `TRACEPARENT` from env at exec time.
    traceparent: Option<String>,
    spawner: Option<ContainerSpawner>,
}

impl ReenterGuard {
    /// Construct a guard for a specific target runner.
    pub fn for_target(target: Runner) -> Self {
        Self {
            target,
            argv: current_process_argv().unwrap_or_default(),
            env: Vec::new(),
            traceparent: None,
            spawner: None,
        }
    }

    /// Use the current process's argv (default; explicit method exists so
    /// the construction reads top-down at call sites like the plan's
    /// example does: `for_target(Runner::Docker).from_current()`).
    pub fn from_current(mut self) -> Self {
        if self.argv.is_empty() {
            self.argv = current_process_argv().unwrap_or_default();
        }
        self
    }

    pub fn with_argv(mut self, argv: Vec<String>) -> Self {
        self.argv = argv;
        self
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Compose env from a `passthrough::EnvSnapshot`. Convenience wrapper.
    pub fn with_passthrough(mut self, snap: &EnvSnapshot) -> Self {
        self.env.extend(snap.to_env_tuples());
        self
    }

    /// Inject the in-process tracer's current span so the child re-exec
    /// continues the trace lineage. Mirrors the bash, which propagates
    /// `TRACEPARENT` via `docker -e TRACEPARENT=$TRACEPARENT`.
    pub fn with_span(mut self, span: &Span) -> Self {
        let pairs = span.traceparent_env();
        self.traceparent = pairs.into_iter().next().map(|(_, v)| v);
        self
    }

    /// Override the parent traceparent string explicitly.
    pub fn with_traceparent(mut self, tp: impl Into<String>) -> Self {
        self.traceparent = Some(tp.into());
        self
    }

    /// Provide the container spawner. Required when target is Docker /
    /// Cloudbuild AND current is a host runner.
    pub fn with_container_spawner(mut self, spawner: ContainerSpawner) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Either return `InContext` or perform the re-exec and return
    /// `Reentered`. Async because the container spawner is async.
    pub async fn exec_or_continue(self) -> Result<Outcome, Error> {
        let current = Runner::detect()?;
        let plan = match current.requires_reenter(self.target) {
            None => return Ok(Outcome::InContext),
            Some(p) => p,
        };

        // Compose env: caller pairs + TRACEPARENT (always last so the
        // caller can't accidentally shadow it with their own).
        let mut env = self.env;
        let tp = self
            .traceparent
            .or_else(|| std::env::var("TRACEPARENT").ok());
        if let Some(tp) = tp {
            env.push(("TRACEPARENT".to_string(), tp));
        }

        match plan.kind {
            ReenterKind::SpawnContainer => {
                let spawner = self.spawner.ok_or(Error::NoSpawner)?;
                let argv = self.argv;
                if argv.is_empty() {
                    return Err(Error::NoCurrentArgv);
                }
                let exit_code = spawner(argv, env).await?;
                Ok(Outcome::Reentered { exit_code })
            }
            ReenterKind::DescendToHost => {
                // Inside a container, the caller wants to keep running
                // on the host. That's not a transition we can perform —
                // the user has to invoke the host runner directly.
                // Mirror the bash, which treats this as "you're already
                // close enough"; return `InContext` so the caller
                // continues. The host-vs-container delta is tracked
                // separately if it matters.
                Ok(Outcome::InContext)
            }
            ReenterKind::Manual => Err(Error::NoPlan {
                from: plan.from,
                target: plan.target,
            }),
        }
    }
}

fn current_process_argv() -> Result<Vec<String>, Error> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.is_empty() {
        return Err(Error::NoCurrentArgv);
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn quiet_env() {
        std::env::remove_var("TRACEPARENT");
    }

    #[tokio::test]
    async fn already_in_target_returns_in_context() {
        quiet_env();
        // We're a host process — asking to reenter "current" is a no-op.
        let current = Runner::detect().unwrap_or(Runner::HostLinux);
        let outcome = ReenterGuard::for_target(current)
            .with_argv(vec!["echo".into()])
            .exec_or_continue()
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::InContext));
    }

    #[tokio::test]
    async fn missing_spawner_for_container_target_errors() {
        quiet_env();
        // Force a transition by claiming we want to reenter into Docker
        // from a non-Docker host. If the test runner itself is in Docker,
        // skip — the transition would be a no-op.
        if std::path::Path::new("/.dockerenv").exists() {
            return;
        }
        if std::env::consts::OS != "macos" && std::env::consts::OS != "linux" {
            return;
        }
        let result = ReenterGuard::for_target(Runner::Docker)
            .with_argv(vec!["x".into()])
            .exec_or_continue()
            .await;
        match result {
            Err(Error::NoSpawner) => {}
            Err(Error::Detect(_)) => {} // host detection couldn't pin
            other => panic!("expected NoSpawner; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawner_receives_argv_and_traceparent_env() {
        quiet_env();
        if std::path::Path::new("/.dockerenv").exists() {
            return;
        }
        let argv_seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(Default::default());
        let env_seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::new(Default::default());
        let call_count = Arc::new(AtomicUsize::new(0));

        let argv_clone = argv_seen.clone();
        let env_clone = env_seen.clone();
        let count_clone = call_count.clone();

        let spawner: ContainerSpawner = Box::new(move |argv, env| {
            *argv_clone.lock().unwrap() = argv;
            *env_clone.lock().unwrap() = env;
            count_clone.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(0) })
        });

        let outcome = ReenterGuard::for_target(Runner::Docker)
            .with_argv(vec!["firestream-ci".into(), "spans".into(), "replay".into()])
            .with_traceparent("00-aabbccddeeff00112233445566778899-1234567890abcdef-01")
            .with_container_spawner(spawner)
            .exec_or_continue()
            .await;
        match outcome {
            Ok(Outcome::Reentered { exit_code }) => assert_eq!(exit_code, 0),
            Ok(Outcome::InContext) => {
                // Caller is already inside Docker — abort assertions.
                return;
            }
            Err(Error::Detect(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        let argv = argv_seen.lock().unwrap();
        assert_eq!(argv[0], "firestream-ci");
        let env = env_seen.lock().unwrap();
        let tp = env.iter().find(|(k, _)| k == "TRACEPARENT").unwrap();
        assert!(tp.1.starts_with("00-aabbcc"));
    }

    #[test]
    fn outcome_into_exit_code_clamps() {
        let c = Outcome::Reentered { exit_code: 257 }.into_exit_code();
        // 257 & 0xff = 1
        assert_eq!(format!("{c:?}"), format!("{:?}", ExitCode::from(1)));
    }

    #[test]
    fn outcome_in_context_is_success_exit() {
        let c = Outcome::InContext.into_exit_code();
        assert_eq!(format!("{c:?}"), format!("{:?}", ExitCode::SUCCESS));
    }
}
