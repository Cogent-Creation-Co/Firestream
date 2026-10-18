//! Diagnostic counters and run-info, propagated up to `main()` for exit code.
//!
//! Go reference: `otelcli/diagnostics.go`.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Diagnostics is a place to put things useful for testing and debugging
/// `otel-cli` runs. The only user-facing surface is `otel-cli status`.
///
/// Field names match Go's `Diagnostics` struct so JSON output is wire-compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Diagnostics {
    #[serde(rename = "cli_args")]
    pub cli_args: Vec<String>,
    #[serde(rename = "is_recording")]
    pub is_recording: bool,
    #[serde(rename = "config_file_loaded")]
    pub config_file_loaded: bool,
    #[serde(rename = "number_of_args")]
    pub number_of_args: i32,
    #[serde(rename = "detected_localhost")]
    pub detected_localhost: bool,
    #[serde(rename = "insecure_skip_verify")]
    pub insecure_skip_verify: bool,
    #[serde(rename = "parsed_timeout_ms")]
    pub parsed_timeout_ms: i64,
    pub endpoint: String,
    #[serde(rename = "endpoint_source")]
    pub endpoint_source: String,
    pub error: String,
    #[serde(rename = "exec_exit_code")]
    pub exec_exit_code: i32,
    pub retries: i32,
}

impl Diagnostics {
    /// Set the global error message from an `Err` value, returning the same
    /// error so the caller can chain it. Mirrors Go's `Diag.SetError`.
    pub fn set_error<E: std::fmt::Display>(err: &E) {
        if let Ok(mut g) = global().lock() {
            g.error = err.to_string();
        }
    }
}

static DIAG: OnceLock<Mutex<Diagnostics>> = OnceLock::new();

/// Process-global diagnostics handle. Commands and clients write into this
/// from anywhere; `otel-cli status` reads it back.
pub fn global() -> &'static Mutex<Diagnostics> {
    DIAG.get_or_init(|| Mutex::new(Diagnostics::default()))
}

/// Convenience helper: clone-out the current diagnostics snapshot.
pub fn snapshot() -> Diagnostics {
    global().lock().map(|g| g.clone()).unwrap_or_default()
}

/// Convenience helper: get the exec exit code recorded by `exec` subcommand.
pub fn get_exit_code() -> i32 {
    global().lock().map(|g| g.exec_exit_code).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_returns_same_instance() {
        // mutate via one handle, observe via another
        {
            let mut g = global().lock().unwrap();
            g.retries = 7;
        }
        let snap = snapshot();
        assert_eq!(snap.retries, 7);

        // reset so other tests don't see stale state
        global().lock().unwrap().retries = 0;
    }

    #[test]
    fn set_error_records_message() {
        let err = std::io::Error::other("boom");
        Diagnostics::set_error(&err);
        assert_eq!(snapshot().error, "boom");

        // reset
        global().lock().unwrap().error = String::new();
    }
}
