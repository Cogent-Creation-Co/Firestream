//! gRPC OTLP client.
//!
//! Go reference: `otlpclient/otlp_client_grpc.go`. Mirrors the Go client's
//! lifecycle: a lazy `new(cfg)` constructor, a `start()` that dials the
//! endpoint and builds the `TraceServiceClient`, and `upload_traces` /
//! `stop` mirroring the Go methods.

use std::time::Duration;

use async_trait::async_trait;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use tonic::metadata::{MetadataKey, MetadataValue};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::Code;

use super::retry::backoff_for_attempt;
use super::{ClientError, OtlpClient};
use crate::config::{parse_duration, Config};

/// Max retry attempts. Matches the bound used by the HTTP clients in Phase 4
/// and the Go retry loop's practical ceiling for a 1s-timeout default.
const MAX_ATTEMPTS: u32 = 3;

/// gRPC OTLP client. Mirrors Go's `GrpcClient` in `otlp_client_grpc.go`.
pub struct GrpcClient {
    cfg: Config,
    client: Option<TraceServiceClient<Channel>>,
    timeout: Duration,
}

impl GrpcClient {
    /// Stash the config; do not connect. Mirrors Go's `NewGrpcClient`.
    pub fn new(cfg: &Config) -> Self {
        let timeout = parse_duration(&cfg.timeout).unwrap_or(Duration::from_secs(1));
        Self {
            cfg: cfg.clone(),
            client: None,
            timeout,
        }
    }
}

/// Resolve a CLI-friendly endpoint string (`host:port`, `grpc://host:port`,
/// `grpcs://host:port`, `http://...`, `https://...`) into a URI suitable for
/// `tonic::transport::Endpoint::from_shared`.
fn resolve_grpc_url(cfg: &Config) -> Result<(String, bool), ClientError> {
    let base = if !cfg.traces_endpoint.is_empty() {
        cfg.traces_endpoint.as_str()
    } else {
        cfg.endpoint.as_str()
    };

    if base.is_empty() {
        return Err(ClientError::Transport(
            "no endpoint configured".to_string(),
        ));
    }

    // Tonic understands `http://` / `https://` schemes natively. Map gRPC
    // schemes to those equivalents per the OTel spec: `grpc` → http,
    // `grpcs` → https.
    let (url, is_tls) = if let Some(rest) = base.strip_prefix("grpcs://") {
        (format!("https://{rest}"), true)
    } else if let Some(rest) = base.strip_prefix("grpc://") {
        (format!("http://{rest}"), false)
    } else if base.starts_with("https://") {
        (base.to_string(), true)
    } else if base.starts_with("http://") {
        (base.to_string(), false)
    } else if base.contains("://") {
        // Unknown scheme — pass through and let tonic reject it if invalid.
        (base.to_string(), false)
    } else if cfg.insecure {
        (format!("http://{base}"), false)
    } else {
        // No scheme + secure: default to https.
        (format!("https://{base}"), true)
    };

    Ok((url, is_tls))
}

#[async_trait]
impl OtlpClient for GrpcClient {
    async fn start(&mut self) -> Result<(), ClientError> {
        let (url, is_tls) = resolve_grpc_url(&self.cfg)?;

        let mut endpoint = Endpoint::from_shared(url.clone())
            .map_err(|e| ClientError::Transport(format!("invalid endpoint {url:?}: {e}")))?
            .timeout(self.timeout)
            .connect_timeout(self.timeout);

        if is_tls {
            // TODO(phase 5-followup): tls_ca_cert / tls_client_cert /
            // tls_client_key file loading via rustls-pemfile, and a custom
            // `ServerCertVerifier` to honour `tls_no_verify`. tonic 0.14 has
            // no public "skip cert check" knob short of providing a verifier
            // through `tls_config_with_verifier`. For now we wire up the
            // platform/webpki roots only — users wanting to skip TLS
            // verification can switch to plaintext via `--insecure` or
            // `http://`/`grpc://`.
            let tls = ClientTlsConfig::new().with_enabled_roots();
            endpoint = endpoint
                .tls_config(tls)
                .map_err(|e| ClientError::Transport(format!("tls config failed: {e}")))?;
        }

        let channel = if self.cfg.blocking {
            endpoint
                .connect()
                .await
                .map_err(|e| ClientError::Transport(format!("could not connect: {e}")))?
        } else {
            endpoint.connect_lazy()
        };

        self.client = Some(TraceServiceClient::new(channel));
        Ok(())
    }

    async fn upload_traces(&mut self, spans: Vec<ResourceSpans>) -> Result<(), ClientError> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| ClientError::Transport("client not started".to_string()))?;

        let req_body = ExportTraceServiceRequest {
            resource_spans: spans,
        };

        let mut last_err: Option<ClientError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(backoff_for_attempt(attempt)).await;
            }

            // Fresh request per attempt — tonic consumes it on send.
            let mut req = tonic::Request::new(req_body.clone());
            for (name, value) in &self.cfg.headers {
                // gRPC metadata names must be lowercase ASCII; values must
                // be valid HTTP/2 header bytes. Drop and warn on invalid
                // entries rather than failing the whole upload, matching
                // Go's tolerant `metadata.New(headers)` behaviour (which
                // panics only on garbage but generally lets per-header
                // mistakes slide).
                let lower = name.to_ascii_lowercase();
                let key = match MetadataKey::from_bytes(lower.as_bytes()) {
                    Ok(k) => k,
                    Err(e) => {
                        tracing::warn!(header = name, error = %e, "skipping invalid gRPC header name");
                        continue;
                    }
                };
                let val = match MetadataValue::try_from(value.as_str()) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(header = name, error = %e, "skipping invalid gRPC header value");
                        continue;
                    }
                };
                req.metadata_mut().insert(key, val);
            }

            match client.export(req).await {
                Ok(_) => return Ok(()),
                Err(status) => {
                    if should_retry(&status) {
                        last_err = Some(status_to_error(&status));
                        continue;
                    }
                    return Err(status_to_error(&status));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ClientError::Transport("retries exhausted".to_string())))
    }

    async fn stop(&mut self) -> Result<(), ClientError> {
        // tonic channels clean up on drop.
        Ok(())
    }
}

/// Mirrors Go's `processGrpcStatus` retry decision. Codes copied verbatim:
/// Aborted, Cancelled, DataLoss, DeadlineExceeded, OutOfRange, Unavailable
/// retry unconditionally; ResourceExhausted retries (Go conditions on
/// RetryInfo — we simplify to always-retry per the Phase 5 plan).
fn should_retry(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        Code::Aborted
            | Code::Cancelled
            | Code::DataLoss
            | Code::DeadlineExceeded
            | Code::OutOfRange
            | Code::Unavailable
            | Code::ResourceExhausted
    )
}

fn status_to_error(status: &tonic::Status) -> ClientError {
    ClientError::Status(format!("gRPC {:?}: {}", status.code(), status.message()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
        TraceService, TraceServiceServer,
    };
    use opentelemetry_proto::tonic::collector::trace::v1::{
        ExportTraceServiceRequest, ExportTraceServiceResponse,
    };
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::Server;

    /// A configurable trace receiver. `responses` is a queue of pre-loaded
    /// statuses (popped from the front per request); an `Ok` status maps to a
    /// successful response. When the queue is empty, every request returns Ok.
    struct CountingService {
        count: Arc<Mutex<u32>>,
        responses: Arc<Mutex<VecDeque<tonic::Status>>>,
        captured_metadata: Arc<Mutex<Vec<tonic::metadata::MetadataMap>>>,
    }

    #[tonic::async_trait]
    impl TraceService for CountingService {
        async fn export(
            &self,
            req: tonic::Request<ExportTraceServiceRequest>,
        ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
            *self.count.lock().unwrap() += 1;
            self.captured_metadata
                .lock()
                .unwrap()
                .push(req.metadata().clone());

            if let Some(s) = self.responses.lock().unwrap().pop_front() {
                if s.code() != tonic::Code::Ok {
                    return Err(s);
                }
            }
            Ok(tonic::Response::new(ExportTraceServiceResponse::default()))
        }
    }

    struct ServerHandle {
        addr: SocketAddr,
        shutdown: Option<oneshot::Sender<()>>,
        join: Option<tokio::task::JoinHandle<()>>,
        count: Arc<Mutex<u32>>,
        captured: Arc<Mutex<Vec<tonic::metadata::MetadataMap>>>,
    }

    impl ServerHandle {
        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn count(&self) -> u32 {
            *self.count.lock().unwrap()
        }

        async fn shutdown(mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(j) = self.join.take() {
                let _ = j.await;
            }
        }
    }

    async fn spawn_server(responses: Vec<tonic::Status>) -> ServerHandle {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let count = Arc::new(Mutex::new(0u32));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let svc = CountingService {
            count: count.clone(),
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            captured_metadata: captured.clone(),
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

        ServerHandle {
            addr,
            shutdown: Some(tx),
            join: Some(join),
            count,
            captured,
        }
    }

    fn cfg_pointing_at(url: &str) -> Config {
        let mut cfg = Config::defaults();
        cfg.endpoint = url.to_string();
        cfg.protocol = "grpc".to_string();
        cfg.service_name = "test".to_string();
        cfg.span_name = "phase5-grpc".to_string();
        cfg.timeout = "5s".to_string();
        cfg.insecure = true;
        cfg
    }

    #[tokio::test]
    async fn grpc_basic_export_succeeds() {
        let server = spawn_server(vec![]).await;
        let cfg = cfg_pointing_at(&server.url());
        let mut client = GrpcClient::new(&cfg);
        client.start().await.expect("start");
        let spans = vec![crate::span::build_resource_spans(&cfg)];
        client.upload_traces(spans).await.expect("upload");
        client.stop().await.unwrap();

        assert_eq!(server.count(), 1);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn grpc_retries_on_unavailable() {
        let server = spawn_server(vec![
            tonic::Status::unavailable("try again"),
            tonic::Status::unavailable("try again"),
        ])
        .await;
        let cfg = cfg_pointing_at(&server.url());
        let mut client = GrpcClient::new(&cfg);
        client.start().await.expect("start");
        let spans = vec![crate::span::build_resource_spans(&cfg)];
        client.upload_traces(spans).await.expect("upload should succeed after retries");

        assert_eq!(server.count(), 3, "expected 2 failures + 1 success");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn grpc_does_not_retry_on_invalid_argument() {
        let server = spawn_server(vec![tonic::Status::invalid_argument("bad request")]).await;
        let cfg = cfg_pointing_at(&server.url());
        let mut client = GrpcClient::new(&cfg);
        client.start().await.expect("start");
        let spans = vec![crate::span::build_resource_spans(&cfg)];
        let err = client
            .upload_traces(spans)
            .await
            .expect_err("invalid argument must propagate");
        match err {
            ClientError::Status(msg) => {
                assert!(msg.contains("InvalidArgument"), "got: {msg}");
            }
            other => panic!("expected Status, got {other:?}"),
        }

        assert_eq!(server.count(), 1, "InvalidArgument must NOT be retried");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn grpc_propagates_metadata_headers() {
        let server = spawn_server(vec![]).await;
        let mut cfg = cfg_pointing_at(&server.url());
        cfg.headers.insert("x-trace-source".to_string(), "phase5-test".to_string());
        let mut client = GrpcClient::new(&cfg);
        client.start().await.expect("start");
        let spans = vec![crate::span::build_resource_spans(&cfg)];
        client.upload_traces(spans).await.expect("upload");

        let captured = server.captured.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        let md = &captured[0];
        let val = md
            .get("x-trace-source")
            .expect("x-trace-source must be present");
        assert_eq!(val.to_str().unwrap(), "phase5-test");
        server.shutdown().await;
    }

    #[test]
    fn resolve_url_bare_host_with_insecure_defaults_to_http() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "localhost:4317".to_string();
        cfg.insecure = true;
        let (url, is_tls) = resolve_grpc_url(&cfg).unwrap();
        assert_eq!(url, "http://localhost:4317");
        assert!(!is_tls);
    }

    #[test]
    fn resolve_url_bare_host_secure_defaults_to_https() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "otel.example.com:4317".to_string();
        cfg.insecure = false;
        let (url, is_tls) = resolve_grpc_url(&cfg).unwrap();
        assert_eq!(url, "https://otel.example.com:4317");
        assert!(is_tls);
    }

    #[test]
    fn resolve_url_grpcs_scheme_maps_to_https() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "grpcs://otel.example.com:4317".to_string();
        let (url, is_tls) = resolve_grpc_url(&cfg).unwrap();
        assert_eq!(url, "https://otel.example.com:4317");
        assert!(is_tls);
    }

    #[test]
    fn resolve_url_grpc_scheme_maps_to_http() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "grpc://localhost:4317".to_string();
        let (url, is_tls) = resolve_grpc_url(&cfg).unwrap();
        assert_eq!(url, "http://localhost:4317");
        assert!(!is_tls);
    }

    #[test]
    fn resolve_url_empty_errors() {
        let cfg = Config::defaults();
        let err = resolve_grpc_url(&cfg).unwrap_err();
        match err {
            ClientError::Transport(_) => {}
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn should_retry_matches_go_codes() {
        assert!(should_retry(&tonic::Status::aborted("")));
        assert!(should_retry(&tonic::Status::cancelled("")));
        assert!(should_retry(&tonic::Status::data_loss("")));
        assert!(should_retry(&tonic::Status::deadline_exceeded("")));
        assert!(should_retry(&tonic::Status::out_of_range("")));
        assert!(should_retry(&tonic::Status::unavailable("")));
        assert!(should_retry(&tonic::Status::resource_exhausted("")));

        assert!(!should_retry(&tonic::Status::invalid_argument("")));
        assert!(!should_retry(&tonic::Status::not_found("")));
        assert!(!should_retry(&tonic::Status::permission_denied("")));
        assert!(!should_retry(&tonic::Status::unauthenticated("")));
        assert!(!should_retry(&tonic::Status::internal("")));
        assert!(!should_retry(&tonic::Status::unimplemented("")));
    }
}
