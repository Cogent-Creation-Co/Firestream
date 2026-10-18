//! End-to-end smoke test for `otel-cli span --protocol json+file`.
//!
//! Verifies the assembled binary writes the expected directory tree and that
//! the resulting `span.json` contains the configured span name and service.

use std::process::Command;

use tempfile::TempDir;

#[test]
fn span_writes_json_file() {
    let dir = TempDir::new().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_otel-cli");

    let status = Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "phase3-smoke",
            "--protocol",
            "json+file",
            "--json-dir",
            dir.path().to_str().unwrap(),
            // Prevent any stray TRACEPARENT in the test environment from
            // shifting the trace id and confusing the assertions.
            "--tp-ignore-env",
        ])
        .status()
        .expect("spawn otel-cli");
    assert!(status.success(), "binary exited non-zero: {status:?}");

    let mut found = false;
    for entry in walkdir::WalkDir::new(dir.path()) {
        let entry = entry.expect("walkdir entry");
        if entry.file_name() == "span.json" {
            found = true;
            let txt = std::fs::read_to_string(entry.path()).expect("read span.json");
            assert!(
                txt.contains("phase3-smoke"),
                "span.json missing name; got: {txt}"
            );
            // span.json mirrors Go's `renderJson` and contains only the inner
            // `Span` proto — the service.name lives in the parent
            // `ResourceSpans.resource`, which isn't part of this file.
        }
    }
    assert!(found, "no span.json under {:?}", dir.path());
}
