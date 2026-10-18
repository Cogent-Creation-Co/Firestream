//! GNU `timeout` / `gtimeout` / passthrough shim. Mirrors
//! `bin/_lib.sh::portable_timeout` (lines 26-33).
//!
//! Resolution order (matches the bash, which is the spec):
//!   1. `timeout`  — GNU coreutils on Linux, optional on macOS via brew.
//!   2. `gtimeout` — Homebrew GNU coreutils on macOS, prefixed to avoid
//!      colliding with macOS's native BSD `timeout` (different flag set).
//!   3. None       — fall back to running the command as-is. The bash
//!      spec silently drops the timeout in this case; we mirror it but
//!      expose it as a typed variant so callers can warn.
//!
//! Pure Rust timeouts (`tokio::time::timeout` around a child future) are
//! NOT a drop-in replacement for `timeout(1)`: the GNU binary sends SIGTERM
//! to the entire process group on expiry, which is the correct behaviour
//! for child shells that fork further. `tokio::time::timeout` only abandons
//! the future. The `exec` module composes both: this shim for the program
//! delegation, `tokio::time::timeout` for the outer kill-watchdog.

use std::process::Command;

/// Result of resolving the host's portable timeout binary at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortableTimeout {
    /// `timeout` is on PATH — typical Linux + Nix devShell.
    Timeout,
    /// `gtimeout` is on PATH — typical macOS with brew coreutils.
    GTimeout,
    /// No timeout binary; commands run without a wrapper.
    Passthrough,
}

impl PortableTimeout {
    /// Probe PATH once. Cache the result in the caller; the bash spec also
    /// resolves once at source-time, not per call.
    pub fn detect() -> Self {
        if which("timeout") {
            Self::Timeout
        } else if which("gtimeout") {
            Self::GTimeout
        } else {
            Self::Passthrough
        }
    }

    pub fn program(&self) -> Option<&'static str> {
        match self {
            Self::Timeout => Some("timeout"),
            Self::GTimeout => Some("gtimeout"),
            Self::Passthrough => None,
        }
    }
}

/// Convenience: which timeout program (if any) should this host use?
pub fn portable_timeout_program() -> Option<&'static str> {
    PortableTimeout::detect().program()
}

/// Wrap a synchronous std `Command` so it runs under the portable timeout
/// binary, dropping back to the command itself if no timeout is available.
/// The wrapper builds a fresh `Command` rather than mutating the input — the
/// caller may have already configured env/cwd/etc., which would be lost if
/// we tried to splice `timeout` in front.
pub fn run_with_timeout(
    secs: u64,
    program: &str,
    args: &[&str],
) -> std::io::Result<std::process::ExitStatus> {
    match PortableTimeout::detect() {
        PortableTimeout::Timeout | PortableTimeout::GTimeout => {
            let mut cmd = Command::new(PortableTimeout::detect().program().expect("just matched"));
            cmd.arg(secs.to_string()).arg(program).args(args);
            cmd.status()
        }
        PortableTimeout::Passthrough => Command::new(program).args(args).status(),
    }
}

fn which(prog: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    std::env::split_paths(&path).any(|dir| dir.join(prog).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_some_variant() {
        let pt = PortableTimeout::detect();
        assert!(matches!(
            pt,
            PortableTimeout::Timeout | PortableTimeout::GTimeout | PortableTimeout::Passthrough
        ));
    }

    #[test]
    fn program_matches_variant() {
        assert_eq!(PortableTimeout::Timeout.program(), Some("timeout"));
        assert_eq!(PortableTimeout::GTimeout.program(), Some("gtimeout"));
        assert_eq!(PortableTimeout::Passthrough.program(), None);
    }
}
