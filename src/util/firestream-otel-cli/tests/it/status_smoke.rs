//! End-to-end smoke tests for `otel-cli status`.
//!
//! Exercises the JSON report shape, canary fanout via the json+file client,
//! and the `detected_localhost` heuristic.

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn run_status(args: &[&str]) -> (std::process::Output, Value) {
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let output = Command::new(bin)
        .arg("status")
        .args(args)
        // Scrub anything that would otherwise leak from the surrounding
        // environment into the report and break assertions.
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_PROTOCOL")
        .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_INSECURE")
        .env_remove("OTEL_CLI_SERVICE_NAME")
        .env_remove("OTEL_SERVICE_NAME")
        .env_remove("TRACEPARENT")
        .output()
        .expect("spawn otel-cli status");

    assert!(
        output.status.success(),
        "non-zero exit: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is utf-8");
    let json: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nbody: {stdout}"));
    (output, json)
}

#[test]
fn status_with_no_endpoint_emits_json() {
    let (_out, json) = run_status(&["--canary-count", "0", "--tp-ignore-env"]);

    // Top-level shape
    for key in &["config", "diagnostics", "env", "cli_args", "span_data"] {
        assert!(
            json.get(*key).is_some(),
            "missing top-level key {key} in {json}"
        );
    }
    // With no endpoint, is_recording must be false.
    assert_eq!(
        json["diagnostics"]["is_recording"],
        Value::Bool(false),
        "is_recording mismatch in {json}"
    );
}

#[test]
fn status_with_json_file_endpoint_writes_canary() {
    let dir = TempDir::new().expect("tempdir");
    let dir_str = dir.path().to_str().unwrap();
    let (_out, json) = run_status(&[
        "--service",
        "test",
        "--protocol",
        "json+file",
        "--json-dir",
        dir_str,
        "--canary-count",
        "2",
        "--tp-ignore-env",
    ]);

    assert_eq!(
        json["diagnostics"]["is_recording"],
        Value::Bool(true),
        "expected recording mode in {json}"
    );

    // Count span.json files in the directory tree — one per canary.
    let mut span_files = 0;
    for entry in walkdir::WalkDir::new(dir.path()) {
        let entry = entry.expect("walkdir entry");
        if entry.file_name() == "span.json" {
            span_files += 1;
        }
    }
    assert_eq!(span_files, 2, "expected 2 span.json files in {dir_str}");
}

#[test]
fn status_detects_localhost() {
    let (_out, json) = run_status(&[
        "--endpoint",
        "http://localhost:4317",
        "--insecure",
        "--canary-count",
        "0",
        "--tp-ignore-env",
    ]);

    assert_eq!(
        json["diagnostics"]["detected_localhost"],
        Value::Bool(true),
        "expected detected_localhost=true in {json}"
    );
}
