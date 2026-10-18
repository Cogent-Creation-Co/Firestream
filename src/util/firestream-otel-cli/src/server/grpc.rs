//! gRPC OTLP receiver — accepts `ExportTraceServiceRequest` over tonic and
//! pushes the spans into a shared `SpanSink`.
//!
//! Go reference: `otlpserver/grpcserver.go`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use tokio::sync::oneshot;
use tonic::transport::Server;

use super::{count_spans, SpanCounter, SpanSink};

/// gRPC service handler implementing the OTLP `TraceService::export` RPC.
struct OtlpTraceService {
    sink: Arc<SpanSink>,
    counter: Arc<SpanCounter>,
}

#[tonic::async_trait]
impl TraceService for OtlpTraceService {
    async fn export(
        &self,
        req: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let spans = req.into_inner().resource_spans;
        let n = count_spans(&spans);
        if let Err(e) = self.sink.ingest(spans).await {
            tracing::warn!(error = %e, "grpc receiver: sink rejected spans");
        }
        self.counter.add(n);
        Ok(tonic::Response::new(ExportTraceServiceResponse::default()))
    }
}

/// Bind and serve the OTLP gRPC receiver on `addr` until `shutdown` fires.
pub async fn run(
    addr: SocketAddr,
    sink: Arc<SpanSink>,
    counter: Arc<SpanCounter>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let svc = OtlpTraceService { sink, counter };

    tracing::info!(addr = %addr, "otel-cli server: gRPC listening");
    eprintln!("otel-cli server: grpc listening on {addr}");

    Server::builder()
        .add_service(TraceServiceServer::new(svc))
        .serve_with_shutdown(addr, async move {
            let _ = shutdown.await;
        })
        .await
        .with_context(|| format!("gRPC server on {addr} failed"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    fn make_span(name: &str) -> ResourceSpans {
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

    #[tokio::test]
    async fn grpc_server_receives_spans() {
        // Pick a free port without holding the listener.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        drop(listener);

        let (tx, mut rx) = mpsc::channel::<ResourceSpans>(8);
        let sink = Arc::new(SpanSink::Channel(tx));
        let counter = SpanCounter::new(0);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let server_sink = sink.clone();
        let server_counter = counter.clone();
        let server_task = tokio::spawn(async move {
            run(addr, server_sink, server_counter, shutdown_rx).await
        });

        // Tiny wait for the server to bind. tonic's serve_with_shutdown does
        // a blocking bind before returning the future, but we still race the
        // task scheduler.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let mut client = TraceServiceClient::connect(format!("http://{addr}"))
            .await
            .expect("client connect");
        let req = ExportTraceServiceRequest {
            resource_spans: vec![make_span("grpc-recv-test")],
        };
        client.export(req).await.expect("export");

        let rs = rx.recv().await.expect("sink got span");
        assert_eq!(rs.scope_spans[0].spans[0].name, "grpc-recv-test");

        let _ = shutdown_tx.send(());
        let _ = server_task.await;
    }
}
