//! Graceful subprocess shutdown. Equivalent of Python `ensure_stop()` at
//! `__init__.py:687-705`: try SIGTERM, wait up to `wait_timeout`, then SIGKILL.

use std::time::Duration;

use tokio::process::Child;

#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;

const DEFAULT_WAIT: Duration = Duration::from_secs(3);

/// Send SIGTERM to `child`, then await its exit up to `wait_timeout`. Falls
/// back to SIGKILL on timeout.
pub async fn ensure_stop(child: &mut Child, cmd_display: &str) {
    ensure_stop_with(child, cmd_display, DEFAULT_WAIT).await
}

#[cfg(unix)]
pub async fn ensure_stop_with(child: &mut Child, cmd_display: &str, wait_timeout: Duration) {
    let sig = Signal::SIGTERM;
    // If already exited, nothing to do.
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(_) => return,
    }

    let Some(pid_raw) = child.id() else { return };
    let pid = Pid::from_raw(pid_raw as i32);
    let _ = kill(pid, sig);

    match tokio::time::timeout(wait_timeout, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            tracing::warn!("Failed to stop process {cmd_display}. Killing it.");
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

#[cfg(not(unix))]
pub async fn ensure_stop_with(child: &mut Child, _cmd_display: &str, _wait_timeout: Duration) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Build a shell-quoted display string for a command (for logging).
pub fn display_cmd(cmd: &[String]) -> String {
    shlex::try_join(cmd.iter().map(String::as_str)).unwrap_or_else(|_| cmd.join(" "))
}
