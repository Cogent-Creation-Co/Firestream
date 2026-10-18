//! Smoke test for `otel-cli memory-sampler`.
//!
//! Spawns the subcommand against the test process's own PID, lets it run
//! long enough to write a few samples, then SIGTERMs it. Asserts that the
//! samples file is non-empty, has monotonic timestamps, and contains
//! plausible RSS values. Runs on both Linux and macOS — the platform-
//! specific RSS reader is exercised by virtue of running the binary.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

#[test]
fn sampler_writes_periodic_samples() {
    let dir = TempDir::new().expect("tempdir");
    let samples = dir.path().join("samples.jsonl");
    let peaks = dir.path().join("peaks.jsonl");

    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let me = std::process::id() as i32;

    let mut child = Command::new(bin)
        .args([
            "memory-sampler",
            "--watch-pid",
            &me.to_string(),
            "--samples-file",
            samples.to_str().unwrap(),
            "--peaks-file",
            peaks.to_str().unwrap(),
            "--interval-ms",
            "200",
            // Disable pressure events for this smoke test: 0 means never.
            "--soft-mb",
            "0",
            "--hard-mb",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn memory-sampler");

    // 1.5s window → ~7 ticks at 200ms after skipping the first; we conservatively
    // require ≥3.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = child.kill();
    let _ = child.wait();

    let f = std::fs::File::open(&samples).expect("samples file exists");
    let rows: Vec<String> = BufReader::new(f).lines().map_while(Result::ok).collect();
    assert!(
        rows.len() >= 3,
        "expected ≥3 samples, got {} ({:?})",
        rows.len(),
        rows
    );

    let mut last_ts: u64 = 0;
    for row in &rows {
        // Parse "ts" and "rss_kb" the same flat-JSON way the consumer does.
        let ts = extract_u64(row, "ts").unwrap_or_else(|| panic!("no ts in {row:?}"));
        let rss_kb = extract_u64(row, "rss_kb").unwrap_or_else(|| panic!("no rss_kb in {row:?}"));
        assert!(ts > last_ts, "ts not monotonic: {ts} ≤ {last_ts}");
        assert!(rss_kb > 0, "rss_kb should be non-zero for a live process");
        last_ts = ts;
    }
}

fn extract_u64(line: &str, name: &str) -> Option<u64> {
    let key = format!("\"{name}\":");
    let i = line.find(&key)?;
    let rest = &line[i + key.len()..];
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}
