//! End-to-end gRPC smoke test: spin up an in-process tonic trace receiver,
//! invoke the `otel-cli` binary via `Command` pointing at it, and verify the
//! request lands with the expected span name.
//!
//! Mirrors `http_smoke.rs` but for the `grpc` protocol.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

#[derive(Default, Clone)]
struct Captured {
    count: u32,
    first_span_name: Option<String>,
}

struct RecordingService {
    captured: Arc<Mutex<Captured>>,
}

#[tonic::async_trait]
impl TraceService for RecordingService {
    async fn export(
        &self,
        req: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let mut c = self.captured.lock().unwrap();
        c.count += 1;
        if c.first_span_name.is_none() {
            for rs in &req.get_ref().resource_spans {
                for ss in &rs.scope_spans {
                    if let Some(span) = ss.spans.first() {
                        c.first_span_name = Some(span.name.clone());
                        return Ok(tonic::Response::new(ExportTraceServiceResponse::default()));
                    }
                }
            }
        }
        Ok(tonic::Response::new(ExportTraceServiceResponse::default()))
    }
}

async fn spawn_recording_server() -> (
    SocketAddr,
    Arc<Mutex<Captured>>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let captured = Arc::new(Mutex::new(Captured::default()));
    let svc = RecordingService {
        captured: captured.clone(),
    };
    let (tx, rx) = oneshot::channel::<()>();
    let incoming = TcpListenerStream::new(listener);
    let join = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(TraceServiceServer::new(svc))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await;
    });
    (addr, captured, tx, join)
}

#[tokio::test]
async fn grpc_smoke() {
    let (addr, captured, shutdown, join) = spawn_recording_server().await;
    let endpoint = format!("http://{addr}");

    let bin = env!("CARGO_BIN_EXE_otel-cli");
    let status = tokio::process::Command::new(bin)
        .args([
            "span",
            "--service",
            "test",
            "--name",
            "grpc-smoke",
            "--endpoint",
            &endpoint,
            "--protocol",
            "grpc",
            "--tp-ignore-env",
            "--fail",
        ])
        .status()
        .await
        .expect("spawn otel-cli");

    assert!(status.success(), "otel-cli exited non-zero: {status:?}");

    let snap = captured.lock().unwrap().clone();
    assert_eq!(snap.count, 1, "expected exactly one gRPC export");
    assert_eq!(
        snap.first_span_name.as_deref(),
        Some("grpc-smoke"),
        "span name mismatch"
    );

    let _ = shutdown.send(());
    let _ = join.await;
}
