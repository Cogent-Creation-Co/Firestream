//! HTTP OTLP receiver — accepts `application/x-protobuf` and
//! `application/json` POSTs at `/v1/traces` and pushes the spans into a
//! shared `SpanSink`.
//!
//! Go reference: `otlpserver/httpserver.go`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::{count_spans, SpanCounter, SpanSink};

#[derive(Clone)]
struct HttpState {
    sink: Arc<SpanSink>,
    counter: Arc<SpanCounter>,
}

/// Bind and serve the OTLP HTTP receiver on `addr` until `shutdown` fires.
pub async fn run(
    addr: SocketAddr,
    sink: Arc<SpanSink>,
    counter: Arc<SpanCounter>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let state = HttpState { sink, counter };

    let app = Router::new()
        .route("/v1/traces", post(handle_traces))
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind http {addr}"))?;
    let bound = listener.local_addr().unwrap_or(addr);
    tracing::info!(addr = %bound, "otel-cli server: HTTP listening");
    eprintln!("otel-cli server: http listening on {bound}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
        .with_context(|| format!("HTTP server on {addr} failed"))?;

    Ok(())
}

async fn handle_traces(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Mirror Go's `ServeHTTP` behaviour: try the explicit content-type first,
    // and if unrecognised, fall back to protobuf (Go's switch defaults to a
    // 406, but the plan says: try protobuf first, then JSON, on unknown).
    let req_result: Result<ExportTraceServiceRequest, String> = if ct.contains("json") {
        serde_json::from_slice::<ExportTraceServiceRequest>(&body)
            .map_err(|e| format!("json decode: {e}"))
    } else if ct.contains("protobuf") {
        ExportTraceServiceRequest::decode(body.as_ref())
            .map_err(|e| format!("protobuf decode: {e}"))
    } else {
        ExportTraceServiceRequest::decode(body.as_ref())
            .map_err(|e| format!("protobuf decode (fallback): {e}"))
            .or_else(|_| {
                serde_json::from_slice::<ExportTraceServiceRequest>(&body)
                    .map_err(|e| format!("json decode (fallback): {e}"))
            })
    };

    let req = match req_result {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = e, "http receiver: bad request body");
            return StatusCode::BAD_REQUEST;
        }
    };

    let spans = req.resource_spans;
    let n = count_spans(&spans);
    if let Err(e) = state.sink.ingest(spans).await {
        tracing::warn!(error = %e, "http receiver: sink rejected spans");
    }
    state.counter.add(n);
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    use tokio::net::TcpListener as TokTcp;
    use tokio::sync::mpsc;

    fn make_rs(name: &str) -> ResourceSpans {
        ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![Span {
                    trace_id: hex::decode("0af7651916cd43dd8448eb211c80319c").unwrap(),
                    span_id: hex::decode("b7ad6b7169203331").unwrap(),
                    name: name.into(),
                    kind: 1,
                    start_time_unix_nano: 1_000,
                    end_time_unix_nano: 2_000,
                    ..Default::default()
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }
    }

    async fn spawn_http_server() -> (
        SocketAddr,
        mpsc::Receiver<ResourceSpans>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let listener = TokTcp::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let (tx, rx) = mpsc::channel::<ResourceSpans>(8);
        let sink = Arc::new(SpanSink::Channel(tx));
        let counter = SpanCounter::new(0);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle =
            tokio::spawn(async move { run(addr, sink, counter, shutdown_rx).await });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        (addr, rx, shutdown_tx, handle)
    }

    #[tokio::test]
    async fn http_server_accepts_protobuf() {
        let (addr, mut rx, shutdown_tx, handle) = spawn_http_server().await;

        let req = ExportTraceServiceRequest {
            resource_spans: vec![make_rs("http-proto-srv")],
        };
        let body = req.encode_to_vec();
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/v1/traces"))
            .header("content-type", "application/x-protobuf")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let got = rx.recv().await.expect("sink got span");
        assert_eq!(got.scope_spans[0].spans[0].name, "http-proto-srv");

        let _ = shutdown_tx.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn http_server_accepts_json() {
        let (addr, mut rx, shutdown_tx, handle) = spawn_http_server().await;

        let req = ExportTraceServiceRequest {
            resource_spans: vec![make_rs("http-json-srv")],
        };
        let body = serde_json::to_vec(&req).unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/v1/traces"))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let got = rx.recv().await.expect("sink got span");
        assert_eq!(got.scope_spans[0].spans[0].name, "http-json-srv");

        let _ = shutdown_tx.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn http_server_falls_back_to_protobuf_on_missing_content_type() {
        let (addr, mut rx, shutdown_tx, handle) = spawn_http_server().await;

        let req = ExportTraceServiceRequest {
            resource_spans: vec![make_rs("http-fallback")],
        };
        let body = req.encode_to_vec();
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/v1/traces"))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let got = rx.recv().await.expect("sink got span");
        assert_eq!(got.scope_spans[0].spans[0].name, "http-fallback");

        let _ = shutdown_tx.send(());
        let _ = handle.await;
    }
}
