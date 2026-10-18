//! HTTP/JSON OTLP client (otel-cli-rs extension).
//!
//! Sends the same `ExportTraceServiceRequest` payload but serialized as JSON
//! via `opentelemetry-proto`'s `with-serde` feature. Endpoint resolution,
//! retry, TLS, and timeout semantics match `HttpProtoClient` exactly — only
//! the `Content-Type` and body encoding differ.

use async_trait::async_trait;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use reqwest::header::HeaderMap;
use url::Url;

use super::http_common::{build_reqwest_client, resolve_url, send_with_retry};
use super::{ClientError, OtlpClient};
use crate::config::Config;

const CONTENT_TYPE: &str = "application/json";

pub struct HttpJsonClient {
    cfg: Config,
    client: Option<reqwest::Client>,
    url: Option<Url>,
    headers: Option<HeaderMap>,
}

impl HttpJsonClient {
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
impl OtlpClient for HttpJsonClient {
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
        let body = serde_json::to_vec(&req)
            .map_err(|e| ClientError::Marshal(format!("json encode: {e}")))?;
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
        cfg.protocol = "http/json".to_string();
        cfg.service_name = "test".to_string();
        cfg.span_name = "phase4-http-json".to_string();
        cfg.timeout = "5s".to_string();
        cfg
    }

    #[tokio::test]
    async fn posts_json_body_to_v1_traces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = cfg_pointing_at(&server.uri());
        let mut client = HttpJsonClient::new(&cfg);
        client.start().await.unwrap();
        let rs = build_resource_spans(&cfg);
        client.upload_traces(vec![rs]).await.unwrap();
        client.stop().await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let parsed: serde_json::Value =
            serde_json::from_slice(&received[0].body).expect("body parses as JSON");
        assert!(
            parsed.get("resourceSpans").is_some(),
            "expected resourceSpans key, got: {parsed}"
        );
    }
}
