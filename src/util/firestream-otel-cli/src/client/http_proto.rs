//! HTTP/protobuf OTLP client.
//!
//! Go reference: `otlpclient/otlp_client_http.go`. Lazy construction — `new`
//! just stashes a clone of the config; `start` materialises the reqwest
//! client and resolves the URL so callers can keep `build_client` synchronous
//! and infallible.

use async_trait::async_trait;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use prost::Message;
use reqwest::header::HeaderMap;
use url::Url;

use super::http_common::{build_reqwest_client, resolve_url, send_with_retry};
use super::{ClientError, OtlpClient};
use crate::config::Config;

const CONTENT_TYPE: &str = "application/x-protobuf";

/// HTTP/protobuf OTLP client. Mirrors Go's `HttpClient` for the
/// `application/x-protobuf` content type.
pub struct HttpProtoClient {
    cfg: Config,
    client: Option<reqwest::Client>,
    url: Option<Url>,
    headers: Option<HeaderMap>,
}

impl HttpProtoClient {
    pub fn new(cfg: &Config) -> Self {
        Self {
            cfg: cfg.clone(),
            client: None,
            url: None,
            headers: None,
        }
    }
}

#[async_trait]
impl OtlpClient for HttpProtoClient {
    async fn start(&mut self) -> Result<(), ClientError> {
        let url = resolve_url(&self.cfg)?;
        let (client, headers, _timeout) = build_reqwest_client(&self.cfg, CONTENT_TYPE)?;
        self.client = Some(client);
        self.url = Some(url);
        self.headers = Some(headers);
        Ok(())
    }

    async fn upload_traces(&mut self, spans: Vec<ResourceSpans>) -> Result<(), ClientError> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| ClientError::Transport("client not started".to_string()))?;
        let url = self
            .url
            .as_ref()
            .ok_or_else(|| ClientError::Transport("client not started".to_string()))?;
        let headers = self
            .headers
            .as_ref()
            .ok_or_else(|| ClientError::Transport("client not started".to_string()))?;

        let req = ExportTraceServiceRequest {
            resource_spans: spans,
        };
        let body = req.encode_to_vec();
        send_with_retry(client, url, headers, body).await
    }

    async fn stop(&mut self) -> Result<(), ClientError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::build_resource_spans;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg_pointing_at(uri: &str) -> Config {
        let mut cfg = Config::defaults();
        cfg.endpoint = uri.to_string();
        cfg.protocol = "http/protobuf".to_string();
        cfg.service_name = "test".to_string();
        cfg.span_name = "phase4-http-proto".to_string();
        // tighten timeout so a hung request doesn't stall the test
        cfg.timeout = "5s".to_string();
        cfg
    }

    #[tokio::test]
    async fn posts_protobuf_body_to_v1_traces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .and(header("content-type", "application/x-protobuf"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let mut client = HttpProtoClient::new(&cfg);
        client.start().await.unwrap();
        let rs = build_resource_spans(&cfg);
        client.upload_traces(vec![rs]).await.unwrap();
        client.stop().await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        assert!(!received[0].body.is_empty(), "body must be non-empty");
    }

    #[tokio::test]
    async fn retries_on_503() {
        let server = MockServer::start().await;
        // Wiremock plays mounts in registration order; mount the failing one
        // first with a count of 2, then the success.
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let mut client = HttpProtoClient::new(&cfg);
        client.start().await.unwrap();
        let rs = build_resource_spans(&cfg);
        client.upload_traces(vec![rs]).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 3, "should retry twice then succeed");
    }

    #[tokio::test]
    async fn returns_status_error_on_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let mut client = HttpProtoClient::new(&cfg);
        client.start().await.unwrap();
        let rs = build_resource_spans(&cfg);
        let err = client.upload_traces(vec![rs]).await.unwrap_err();
        match err {
            ClientError::Status(msg) => assert!(msg.contains("400"), "got: {msg}"),
            other => panic!("expected Status(400), got {other:?}"),
        }

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1, "4xx must NOT be retried");
    }
}
