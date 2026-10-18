//! Phase 7: end-to-end server modes — `otel-cli server json` receives spans
//! from the `otel-cli span` binary acting as client, then writes them to
//! disk. The headline test (`json_client_and_server_produce_byte_equivalent_output`)
//! pins down the contract that the `json+file` client and `server json`
//! produce identical on-disk artefacts for the same input.

use std::net::{SocketAddr, TcpListener as StdTcp};
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// Pick an unused TCP port by binding 127.0.0.1:0 and immediately releasing
/// the listener. There is a small race window before the server claims it,
/// but in practice this is the same approach the existing http/grpc smoke
/// tests use to spin up wiremock-style fixtures.
fn pick_free_port() -> u16 {
    let l = StdTcp::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().expect("local_addr").port();
    drop(l);
    port
}

/// Spawn `otel-cli server json` and wait for its "listening" stderr line.
async fn spawn_server_json(
    proto: &str,
    addr: SocketAddr,
    out_dir: &Path,
    max_spans: u32,
) -> tokio::process::Child {
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let mut cmd = Command::new(bin);
    cmd.arg("server");
    if proto == "grpc" {
        cmd.args(["--grpc-addr", &addr.to_string(), "--http-addr", ""]);
    } else {
        cmd.args(["--http-addr", &addr.to_string(), "--grpc-addr", ""]);
    }
    cmd.args([
        "--max-spans",
        &max_spans.to_string(),
        "json",
        "--dir",
        out_dir.to_str().unwrap(),
    ]);
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn otel-cli server");

    // Wait for stderr to emit "listening on" so we know the bind succeeded.
    if let Some(stderr) = child.stderr.take() {
        let mut reader = tokio::io::BufReader::new(stderr).lines();
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("server> {line}");
                if line.contains("listening") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(ready, "server did not report listening within 5s");
    }

    child
}

async fn run_span_client_grpc(addr: SocketAddr, name: &str, extra: &[&str]) {
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let endpoint = format!("http://{addr}");
    let mut args: Vec<&str> = vec![
        "span",
        "--service",
        "e2e",
        "--name",
        name,
        "--endpoint",
        &endpoint,
        "--protocol",
        "grpc",
        "--tp-ignore-env",
        "--fail",
    ];
    args.extend_from_slice(extra);
    let status = Command::new(bin)
        .args(&args)
        .status()
        .await
        .expect("spawn span client");
    assert!(status.success(), "span client failed: {status:?}");
}

async fn run_span_client_http(addr: SocketAddr, name: &str) {
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let endpoint = format!("http://{addr}");
    let status = Command::new(bin)
        .args([
            "span",
            "--service",
            "e2e",
            "--name",
            name,
            "--endpoint",
            &endpoint,
            "--protocol",
            "http/protobuf",
            "--tp-ignore-env",
            "--fail",
        ])
        .status()
        .await
        .expect("spawn span client");
    assert!(status.success(), "span client failed: {status:?}");
}

fn find_span_json(root: &Path) -> Option<std::path::PathBuf> {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_name() == "span.json" {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

#[tokio::test]
async fn server_json_receives_grpc_span_from_client() {
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let mut server = spawn_server_json("grpc", addr, tmp.path(), 1).await;

    run_span_client_grpc(addr, "srv-smoke", &[]).await;

    // Server should exit after one span.
    let status = tokio::time::timeout(Duration::from_secs(10), server.wait())
        .await
        .expect("server did not exit after max_spans=1")
        .expect("server wait");
    assert!(status.success(), "server exited non-zero: {status:?}");

    let span_path = find_span_json(tmp.path()).expect("server should have written span.json");
    let txt = std::fs::read_to_string(&span_path).unwrap();
    assert!(
        txt.contains("srv-smoke"),
        "span.json missing 'srv-smoke': {txt}"
    );
}

#[tokio::test]
async fn server_json_receives_http_span_from_client() {
    let tmp = tempfile::TempDir::new().unwrap();
    let port = pick_free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let mut server = spawn_server_json("http", addr, tmp.path(), 1).await;

    run_span_client_http(addr, "srv-smoke-http").await;

    let status = tokio::time::timeout(Duration::from_secs(10), server.wait())
        .await
        .expect("server did not exit after max_spans=1")
        .expect("server wait");
    assert!(status.success(), "server exited non-zero: {status:?}");

    let span_path = find_span_json(tmp.path()).expect("server should have written span.json");
    let txt = std::fs::read_to_string(&span_path).unwrap();
    assert!(
        txt.contains("srv-smoke-http"),
        "span.json missing 'srv-smoke-http': {txt}"
    );
}

#[tokio::test]
async fn json_client_and_server_produce_byte_equivalent_output() {
    // Force identical IDs on both runs so the only legitimate difference
    // would be a serialisation-layer divergence.
    let trace_id = "0af7651916cd43dd8448eb211c80319c";
    let span_id = "b7ad6b7169203331";

    let dir_a = tempfile::TempDir::new().unwrap();
    let dir_b = tempfile::TempDir::new().unwrap();

    // (A) client-side: otel-cli span --protocol json+file --json-dir DIR_A
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let status = Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "same",
            "--protocol",
            "json+file",
            "--json-dir",
            dir_a.path().to_str().unwrap(),
            "--force-trace-id",
            trace_id,
            "--force-span-id",
            span_id,
            "--tp-ignore-env",
            "--fail",
            // Pin start/end so the JSON is bit-stable.
            "--start",
            "1700000000.000000000",
            "--end",
            "1700000001.000000000",
        ])
        .status()
        .await
        .expect("spawn json+file client");
    assert!(status.success(), "json+file client failed: {status:?}");

    // (B) server side: server-json receives a grpc-shipped span with the same IDs.
    let port = pick_free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut server = spawn_server_json("grpc", addr, dir_b.path(), 1).await;

    let status = Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "same",
            "--endpoint",
            &format!("http://{addr}"),
            "--protocol",
            "grpc",
            "--force-trace-id",
            trace_id,
            "--force-span-id",
            span_id,
            "--tp-ignore-env",
            "--fail",
            "--start",
            "1700000000.000000000",
            "--end",
            "1700000001.000000000",
        ])
        .status()
        .await
        .expect("spawn grpc client");
    assert!(status.success(), "grpc client failed: {status:?}");

    let _ = tokio::time::timeout(Duration::from_secs(10), server.wait())
        .await
        .expect("server did not exit");

    let path_a = dir_a.path().join(trace_id).join(span_id).join("span.json");
    let path_b = dir_b.path().join(trace_id).join(span_id).join("span.json");
    assert!(path_a.exists(), "{path_a:?} missing");
    assert!(path_b.exists(), "{path_b:?} missing");

    let a: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path_a).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path_b).unwrap()).unwrap();

    // Identity columns: must match exactly.
    for key in &["traceId", "spanId", "name", "kind"] {
        assert_eq!(
            a.get(*key),
            b.get(*key),
            "field {key:?} mismatched between client and server JSON:\n  a={:?}\n  b={:?}",
            a.get(*key),
            b.get(*key)
        );
    }
    // start/end nanos should also match given we forced them.
    for key in &["startTimeUnixNano", "endTimeUnixNano"] {
        assert_eq!(a.get(*key), b.get(*key), "field {key:?} mismatched");
    }
}
