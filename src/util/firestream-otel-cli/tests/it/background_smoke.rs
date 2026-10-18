//! Integration smoke tests for `otel-cli span background/event/end`.
//!
//! Spawns the real binary via `assert_cmd` and walks a small client/server
//! lifecycle, asserting that span.json + event-N.json show up in the json+file
//! output directory.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

const SOCK_FILE: &str = "otel-cli.sock";

fn wait_for_socket(sockdir: &Path, timeout: Duration) -> bool {
    let path = sockdir.join(SOCK_FILE);
    let started = Instant::now();
    while started.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn find_files_named(root: &Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let e = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if e.file_name().to_string_lossy() == name {
            out.push(e.into_path());
        }
    }
    out
}

#[test]
fn background_complete_lifecycle() {
    let sockdir = TempDir::new().unwrap();
    let outdir = TempDir::new().unwrap();

    // Detached background — process forks itself and returns.
    // Note: clap requires parent-command flags (--service, --name, etc.)
    // *before* the `background` subcommand.
    let mut bg = Command::cargo_bin("otel-cli")
        .unwrap()
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "bg-smoke",
            "--protocol",
            "json+file",
            "--json-dir",
            outdir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "background",
            "--sockdir",
            sockdir.path().to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn span background");

    // The foreground process should return immediately (detached); the actual
    // server is a child. Wait for the parent's exit then for the socket to
    // appear.
    let status = bg.wait().expect("wait foreground bg");
    assert!(
        status.success(),
        "foreground span background exited non-zero: {status:?}"
    );

    assert!(
        wait_for_socket(sockdir.path(), Duration::from_secs(5)),
        "background socket never appeared at {:?}",
        sockdir.path()
    );

    // Two events
    for n in ["e1", "e2"] {
        let out = Command::cargo_bin("otel-cli")
            .unwrap()
            .args([
                "span",
                "event",
                "--sockdir",
                sockdir.path().to_str().unwrap(),
                "--name",
                n,
                "--tp-ignore-env",
            ])
            .output()
            .expect("span event");
        assert!(
            out.status.success(),
            "span event {n} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // End
    let out = Command::cargo_bin("otel-cli")
        .unwrap()
        .args([
            "span",
            "end",
            "--sockdir",
            sockdir.path().to_str().unwrap(),
            "--tp-ignore-env",
        ])
        .output()
        .expect("span end");
    assert!(
        out.status.success(),
        "span end failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Give the orphaned background server a moment to flush span.json and its
    // events. span.json and event-N.json are written back-to-back in one
    // server-side call, but they land as separate files; poll until all three
    // are visible rather than racing on span.json alone.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut span_jsons = Vec::new();
    let mut event0 = Vec::new();
    let mut event1 = Vec::new();
    while Instant::now() < deadline {
        span_jsons = find_files_named(outdir.path(), "span.json");
        event0 = find_files_named(outdir.path(), "event-0.json");
        event1 = find_files_named(outdir.path(), "event-1.json");
        if !span_jsons.is_empty() && !event0.is_empty() && !event1.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !span_jsons.is_empty(),
        "expected span.json in {:?}",
        outdir.path()
    );
    assert!(
        !event0.is_empty(),
        "expected event-0.json in {:?}",
        outdir.path()
    );
    assert!(
        !event1.is_empty(),
        "expected event-1.json in {:?}",
        outdir.path()
    );
}

#[test]
fn background_wait_blocks_until_end() {
    let sockdir = TempDir::new().unwrap();
    let outdir = TempDir::new().unwrap();

    // --wait makes the foreground process block until the server exits.
    let mut bg = Command::cargo_bin("otel-cli")
        .unwrap()
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "bg-wait",
            "--protocol",
            "json+file",
            "--json-dir",
            outdir.path().to_str().unwrap(),
            "--tp-ignore-env",
            "background",
            "--wait",
            "--sockdir",
            sockdir.path().to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn span background --wait");

    assert!(
        wait_for_socket(sockdir.path(), Duration::from_secs(10)),
        "background socket never appeared at {:?}",
        sockdir.path()
    );

    // The foreground process should NOT have exited yet.
    if let Some(status) = bg.try_wait().expect("try_wait") {
        panic!("foreground --wait returned early: {status:?}");
    }

    // End from a parallel thread.
    let end_sockdir = sockdir.path().to_path_buf();
    let end_thread = std::thread::spawn(move || {
        // small delay so the test can observe still-running state above
        std::thread::sleep(Duration::from_millis(100));
        let out = Command::cargo_bin("otel-cli")
            .unwrap()
            .args([
                "span",
                "end",
                "--sockdir",
                end_sockdir.to_str().unwrap(),
                "--tp-ignore-env",
            ])
            .output()
            .expect("span end");
        assert!(
            out.status.success(),
            "span end failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    });

    // The foreground should now exit reasonably quickly.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = None;
    while Instant::now() < deadline {
        if let Some(s) = bg.try_wait().expect("try_wait") {
            exited = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    end_thread.join().expect("end thread");

    let status = exited.expect("foreground never exited after span end");
    assert!(
        status.success(),
        "foreground --wait exited non-zero: {status:?}"
    );

    // Verify outputs.
    let span_jsons = find_files_named(outdir.path(), "span.json");
    assert!(
        !span_jsons.is_empty(),
        "expected span.json in {:?}",
        outdir.path()
    );
}
