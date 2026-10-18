//! JSON-to-disk sink used by `otel-cli server json`.
//!
//! Go reference: `otelcli/server_json.go::renderJson`. The actual writing is
//! shared with the `json+file` client through `crate::json_layout`.

use anyhow::Result;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use std::path::PathBuf;

use crate::json_layout::write_resource_spans_to_dir;

/// Writes each incoming `ResourceSpans` under `<dir>/<traceHex>/<spanHex>/`.
pub struct JsonSink {
    pub dir: PathBuf,
}

impl JsonSink {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub async fn write(&self, rs: &ResourceSpans) -> Result<()> {
        // The server-side sink is not the tee's durable leg — callers
        // re-emit through the durable file client when they need fsync.
        write_resource_spans_to_dir(&self.dir, rs, false).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ScopeSpans, Span};

    fn dummy_rs() -> ResourceSpans {
        let span = Span {
            trace_id: hex::decode("0af7651916cd43dd8448eb211c80319c").unwrap(),
            span_id: hex::decode("b7ad6b7169203331").unwrap(),
            name: "json-sink".into(),
            kind: 1,
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
    async fn json_sink_writes_span_to_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sink = JsonSink::new(tmp.path());
        sink.write(&dummy_rs()).await.unwrap();

        let p = tmp
            .path()
            .join("0af7651916cd43dd8448eb211c80319c")
            .join("b7ad6b7169203331")
            .join("span.json");
        assert!(p.exists());
        let txt = tokio::fs::read_to_string(&p).await.unwrap();
        assert!(txt.contains("json-sink"));
    }
}
