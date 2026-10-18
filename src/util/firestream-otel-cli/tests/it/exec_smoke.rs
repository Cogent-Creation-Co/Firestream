//! Phase 6: exec subcommand smoke tests.
//!
//! These use the otel-cli binary in a subprocess pointed at a temp JSON dir
//! so they're hermetic — no network needed.

use std::process::Command;

use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_otel-cli")
}

/// Find the single span.json under `dir` (the JsonFileClient writes
/// `<dir>/<traceHex>/<spanHex>/span.json`).
fn find_span_json(dir: &std::path::Path) -> std::path::PathBuf {
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.expect("walkdir entry");
        if entry.file_name() == "span.json" {
            return entry.path().to_path_buf();
        }
    }
    panic!("no span.json under {dir:?}");
}

#[test]
fn exec_writes_json_with_exit_zero() {
    let dir = TempDir::new().expect("tempdir");
    let output = Command::new(bin())
        .args([
            "exec",
            "--service",
            "test",
            "--name",
            "e",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "--",
            "echo",
            "hello",
        ])
        .output()
        .expect("spawn otel-cli");

    assert!(
        output.status.success(),
        "non-zero exit: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"), "stdout missing 'hello': {stdout}");

    let span_path = find_span_json(dir.path());
    let txt = std::fs::read_to_string(&span_path).expect("read span.json");
    // OTLP/JSON status codes: 1 = UNSET. Confirm we either didn't set status
    // (defaults to UNSET via build_span) or set it explicitly to UNSET. The
    // serialized form may be `"code": 1` (numeric) or `"code": "STATUS_CODE_UNSET"`.
    // We just confirm STATUS_CODE_ERROR is absent.
    assert!(
        !txt.contains("STATUS_CODE_ERROR") && !txt.contains("\"code\": 2"),
        "expected non-error status; got {txt}"
    );
}

#[test]
fn exec_sets_error_status_on_nonzero_exit() {
    let dir = TempDir::new().expect("tempdir");
    let output = Command::new(bin())
        .args([
            "exec",
            "--service",
            "test",
            "--name",
            "e",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "--",
            "sh",
            "-c",
            "exit 7",
        ])
        .output()
        .expect("spawn otel-cli");

    assert_eq!(
        output.status.code(),
        Some(7),
        "expected child exit 7, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let span_path = find_span_json(dir.path());
    let txt = std::fs::read_to_string(&span_path).expect("read span.json");
    // serde of opentelemetry-proto emits enums as the enum name string.
    assert!(
        txt.contains("STATUS_CODE_ERROR") || txt.contains("\"code\": 2"),
        "expected error status; got {txt}"
    );
    assert!(
        txt.contains("exit status 7"),
        "expected exit message; got {txt}"
    );
}

#[test]
fn exec_injects_traceparent_into_env() {
    let dir = TempDir::new().expect("tempdir");
    let output = Command::new(bin())
        .args([
            "exec",
            "--service",
            "test",
            "--name",
            "e",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "--",
            "sh",
            "-c",
            "echo TP=$TRACEPARENT",
        ])
        .output()
        .expect("spawn otel-cli");

    assert!(
        output.status.success(),
        "non-zero exit: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tp_line = stdout
        .lines()
        .find(|l| l.starts_with("TP="))
        .unwrap_or_else(|| panic!("no TP=... line in: {stdout}"));
    let value = tp_line.trim_start_matches("TP=");
    assert!(
        regex_match_traceparent(value),
        "TRACEPARENT not canonical: {value:?}"
    );
}

#[test]
fn exec_substitutes_traceparent_placeholder() {
    let dir = TempDir::new().expect("tempdir");
    let output = Command::new(bin())
        .args([
            "exec",
            "--service",
            "test",
            "--name",
            "e",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "--",
            "sh",
            "-c",
            "echo HDR={{traceparent}}",
        ])
        .output()
        .expect("spawn otel-cli");

    assert!(
        output.status.success(),
        "non-zero exit: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let hdr_line = stdout
        .lines()
        .find(|l| l.starts_with("HDR="))
        .unwrap_or_else(|| panic!("no HDR=... line in: {stdout}"));
    let value = hdr_line.trim_start_matches("HDR=");
    assert!(
        regex_match_traceparent(value),
        "HDR traceparent not canonical: {value:?}"
    );
}

#[test]
fn exec_propagates_child_exit_code() {
    let dir = TempDir::new().expect("tempdir");
    let output = Command::new(bin())
        .args([
            "exec",
            "--service",
            "test",
            "--name",
            "e",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "--",
            "sh",
            "-c",
            "exit 42",
        ])
        .output()
        .expect("spawn otel-cli");

    assert_eq!(
        output.status.code(),
        Some(42),
        "expected child exit 42, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Matches the canonical form `00-<32hex>-<16hex>-01`.
fn regex_match_traceparent(s: &str) -> bool {
    let re = regex::Regex::new(r"^00-[0-9a-f]{32}-[0-9a-f]{16}-01$").unwrap();
    re.is_match(s)
}
