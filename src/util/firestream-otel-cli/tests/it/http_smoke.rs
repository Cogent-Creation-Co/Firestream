//! Smoke test: spawn wiremock, run `otel-cli span` pointing at it,
//! verify the request was received with the expected body shape.

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn http_protobuf_smoke() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .and(header("content-type", "application/x-protobuf"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let endpoint = server.uri();

    // tokio::process is fine here — we're already inside a tokio runtime.
    let status = tokio::process::Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "http-smoke",
            "--endpoint",
            &endpoint,
            "--protocol",
            "http/protobuf",
            "--tp-ignore-env",
            "--fail",
        ])
        .status()
        .await
        .expect("spawn otel-cli");

    assert!(status.success(), "otel-cli exited non-zero: {status:?}");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0].body.is_empty(),
        "protobuf body should be non-empty"
    );
}

#[tokio::test]
async fn http_json_smoke() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let endpoint = server.uri();

    let status = tokio::process::Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "http-json-smoke",
            "--endpoint",
            &endpoint,
            "--protocol",
            "http/json",
            "--tp-ignore-env",
            "--fail",
        ])
        .status()
        .await
        .expect("spawn otel-cli");

    assert!(status.success(), "otel-cli exited non-zero: {status:?}");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let parsed: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("body must be JSON");
    assert!(
        parsed.get("resourceSpans").is_some(),
        "expected resourceSpans key, got: {parsed}"
    );
}

/// Sanity test: the binary needs to be built before tests run. Cargo handles
/// that via `CARGO_BIN_EXE_otel-cli` automatically.
#[test]
fn binary_exists() {
    let bin = env!("CARGO_BIN_EXE_otel-cli");
    assert!(
        std::path::Path::new(bin).exists(),
        "binary not built: {bin}"
    );
}
