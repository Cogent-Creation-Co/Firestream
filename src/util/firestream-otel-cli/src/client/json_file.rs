//! JSON-file OTLP "client" (new — not in the Go upstream).
//!
//! Writes each span as `<dir>/<traceHex>/<spanHex>/span.json` and events as
//! `event-N.json`, matching the directory layout of `otel-cli server json`
//! (`otelcli/server_json.go::renderJson`).
//!
//! The serialization uses the `with-serde` feature of `opentelemetry-proto`,
//! which emits OTLP/JSON: bytes are hex-encoded, u64 nanos are stringified,
//! oneof variants are flattened.

use async_trait::async_trait;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use std::path::PathBuf;

use super::{ClientError, OtlpClient};
use crate::json_layout::write_resource_spans_to_dir;

pub struct JsonFileClient {
    pub dir: PathBuf,
    /// When true, each write is fdatasync'd and the parent directory is fsync'd.
    /// PRD §9.1: required for the durable file leg of the tee exporter.
    pub durable: bool,
}

impl JsonFileClient {
    /// Create a non-durable client. Equivalent to the pre-W5 behaviour.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            durable: false,
        }
    }

    /// Create a client with an explicit durable flag (PRD §9.1).
    pub fn with_durable(dir: impl Into<PathBuf>, durable: bool) -> Self {
        Self {
            dir: dir.into(),
            durable,
        }
    }
}

#[async_trait]
impl OtlpClient for JsonFileClient {
    async fn start(&mut self) -> Result<(), ClientError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        Ok(())
    }

    async fn upload_traces(&mut self, spans: Vec<ResourceSpans>) -> Result<(), ClientError> {
        for rs in &spans {
            write_resource_spans_to_dir(&self.dir, rs, self.durable).await?;
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ClientError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{
        any_value::Value as AnyValueOneof, AnyValue, KeyValue,
    };
    use opentelemetry_proto::tonic::trace::v1::{
        span::Event, ScopeSpans, Span,
    };

    fn make_span_with_events(n_events: usize) -> ResourceSpans {
        let events: Vec<Event> = (0..n_events)
            .map(|i| Event {
                time_unix_nano: 1_000 + i as u64,
                name: format!("event-name-{i}"),
                attributes: vec![KeyValue {
                    key: "k".into(),
                    value: Some(AnyValue {
                        value: Some(AnyValueOneof::StringValue(format!("v-{i}"))),
                    }),
                    ..Default::default()
                }],
                dropped_attributes_count: 0,
            })
            .collect();

        let span = Span {
            trace_id: hex::decode("0af7651916cd43dd8448eb211c80319c").unwrap(),
            span_id: hex::decode("b7ad6b7169203331").unwrap(),
            name: "phase3-test".into(),
            kind: 0,
            start_time_unix_nano: 1_000_000,
            end_time_unix_nano: 2_000_000,
            events,
            ..Default::default()
        };

        ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![span],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }
    }

    #[tokio::test]
    async fn json_file_writes_span() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut client = JsonFileClient::new(tmp.path());
        client.start().await.unwrap();

        let rs = make_span_with_events(2);
        client.upload_traces(vec![rs]).await.unwrap();
        client.stop().await.unwrap();

        let span_dir = tmp
            .path()
            .join("0af7651916cd43dd8448eb211c80319c")
            .join("b7ad6b7169203331");
        let span_path = span_dir.join("span.json");
        assert!(span_path.exists(), "{span_path:?} should exist");

        let txt = tokio::fs::read_to_string(&span_path).await.unwrap();
        assert!(txt.contains("phase3-test"));
        // trace id is serialized as hex
        assert!(txt.contains("0af7651916cd43dd8448eb211c80319c"));

        for i in 0..2 {
            let event_path = span_dir.join(format!("event-{i}.json"));
            assert!(event_path.exists(), "{event_path:?} should exist");
            let etxt = tokio::fs::read_to_string(&event_path).await.unwrap();
            assert!(etxt.contains(&format!("event-name-{i}")));
        }
    }

    #[tokio::test]
    async fn json_file_no_events_writes_only_span() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut client = JsonFileClient::new(tmp.path());
        client.start().await.unwrap();
        client.upload_traces(vec![make_span_with_events(0)]).await.unwrap();

        // span.json present, no stray event files
        let span_dir = tmp
            .path()
            .join("0af7651916cd43dd8448eb211c80319c")
            .join("b7ad6b7169203331");
        let mut entries = tokio::fs::read_dir(&span_dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["span.json".to_string()]);
    }
}
