//! Bounded-wait helper used by harness teardown guards.
//!
//! Lifted from `firestream/tests/e2e/harness.rs` so both harnesses share the
//! same drop-budget semantics: a guard's `Drop` spawns the teardown command,
//! then calls [`wait_with_budget`] to bound how long the destructor blocks
//! before the caller kills the child and falls back to a backend-specific
//! force-cleanup path.

use std::thread;
use std::time::{Duration, Instant};

/// Poll interval used inside [`wait_with_budget`]. Public so callers can size
/// their own budgets in multiples of it.
pub const POLL: Duration = Duration::from_millis(250);

/// Upper bound for one teardown: a guard's `Drop`, or one entry drained by
/// [`crate::cleanup`] after a signal.
pub const TEARDOWN_BUDGET: Duration = Duration::from_secs(60);

/// Wait up to `budget` for `child` to exit. Returns `Ok(())` on clean exit
/// (any exit status), `Err(())` on timeout or on a `try_wait` IO error. On
/// timeout the caller is responsible for killing the child and reaping.
pub fn wait_with_budget(child: &mut std::process::Child, budget: Duration) -> Result<(), ()> {
    let start = Instant::now();
    while start.elapsed() < budget {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => thread::sleep(POLL),
            Err(_) => return Err(()),
        }
    }
    Err(())
}
