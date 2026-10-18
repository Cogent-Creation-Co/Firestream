//! Exit-code interpretation. Mirrors `bin/_lib.sh::describe_exit` (lines
//! 151-159) plus the OOM-classification convention used by `run_with_log`.
//!
//! Three exit codes carry signal information in POSIX shells:
//!   137 = 128 + 9   SIGKILL (kernel OOM-killer or explicit `kill -9`)
//!   139 = 128 + 11  SIGSEGV
//!   143 = 128 + 15  SIGTERM
//!
//! Returning `&'static str` rather than `String` so the caller can attach
//! to a tracing field without an allocation per command.

/// Human-readable label for an integer exit code. Stable strings — used as
/// span attributes and log keys.
pub fn describe_exit(code: i32) -> &'static str {
    match code {
        0 => "success",
        137 => "OOM killed (SIGKILL)",
        139 => "segfault (SIGSEGV)",
        143 => "terminated (SIGTERM)",
        _ => "non-zero exit",
    }
}

/// True when `code` encodes a signal termination (POSIX `128 + signum`).
pub fn exit_is_signal(code: i32) -> bool {
    (129..=192).contains(&code)
}

/// Extract the signal number if `code` is `128 + signum`, otherwise None.
pub fn signal_from_exit(code: i32) -> Option<i32> {
    if exit_is_signal(code) {
        Some(code - 128)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_known_signals() {
        assert_eq!(describe_exit(0), "success");
        assert_eq!(describe_exit(137), "OOM killed (SIGKILL)");
        assert_eq!(describe_exit(139), "segfault (SIGSEGV)");
        assert_eq!(describe_exit(143), "terminated (SIGTERM)");
        assert_eq!(describe_exit(1), "non-zero exit");
    }

    #[test]
    fn signal_classification() {
        assert!(exit_is_signal(137));
        assert!(exit_is_signal(143));
        assert!(!exit_is_signal(0));
        assert!(!exit_is_signal(1));
        assert!(!exit_is_signal(128));
        assert!(!exit_is_signal(193));
    }

    #[test]
    fn extracts_signal_number() {
        assert_eq!(signal_from_exit(137), Some(9));
        assert_eq!(signal_from_exit(143), Some(15));
        assert_eq!(signal_from_exit(0), None);
        assert_eq!(signal_from_exit(1), None);
    }
}
