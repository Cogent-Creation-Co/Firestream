//! RAII `Span` — open via `Tracer::root_span` or `Span::child`, close at
//! `Drop`. Mirrors `bin/_otel.sh::otel_span_open` / `otel_span_close`.

use std::collections::BTreeMap;
use std::time::SystemTime;

use opentelemetry_proto::tonic::common::v1::{
    AnyValue, InstrumentationScope, KeyValue, any_value::Value as AnyValueOneof,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{
    ResourceSpans, ScopeSpans, Span as ProtoSpan, Status, status::StatusCode,
};
use otel_cli::traceparent::Traceparent;

use super::Tracer;

/// Caller-set span status. Default is `Unset` — matches OTel SDK semantics
/// where libraries don't presume to declare success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpanStatus {
    #[default]
    Unset,
    Ok,
    Error,
}

impl SpanStatus {
    fn proto(self) -> StatusCode {
        match self {
            Self::Unset => StatusCode::Unset,
            Self::Ok => StatusCode::Ok,
            Self::Error => StatusCode::Error,
        }
    }
}

/// RAII handle to an open span. Holding it open keeps the span "live";
/// dropping it emits the span to the tracer's checkpoint dir + buffers
/// it for network upload at `Tracer::shutdown`.
pub struct Span {
    // `Tracer` holds a `Mutex<Box<dyn OtlpClient>>`, which is not `Debug`, so
    // `Span` cannot derive it. Hand-rolled below: callers embed spans in
    // `#[derive(Debug)]` context structs.
    tracer: Tracer,
    name: String,
    traceparent: Traceparent,
    parent_span_id: Vec<u8>,
    start_ns: u64,
    attributes: BTreeMap<String, String>,
    status: SpanStatus,
    status_message: String,
    emitted: bool,
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Span")
            .field("name", &self.name)
            .field("trace_id", &hex::encode(self.traceparent.trace_id))
            .field("span_id", &hex::encode(self.traceparent.span_id))
            .field("status", &self.status)
            .field("emitted", &self.emitted)
            .finish_non_exhaustive()
    }
}

impl Span {
    pub(crate) fn new(
        tracer: Tracer,
        name: String,
        tp: Traceparent,
        parent_span_id: Vec<u8>,
    ) -> Self {
        let start_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            tracer,
            name,
            traceparent: tp,
            parent_span_id,
            start_ns,
            attributes: BTreeMap::new(),
            status: SpanStatus::default(),
            status_message: String::new(),
            emitted: false,
        }
    }

    /// Open a child span. The new span shares this span's trace id and
    /// records this span's id as its parent.
    pub fn child(&self, name: impl Into<String>) -> Self {
        let tp = self.traceparent.new_child();
        let parent = self.traceparent.span_id.to_vec();
        Self::new(self.tracer.clone(), name.into(), tp, parent)
    }

    /// Attach a (key, value) attribute. Re-keying overrides the prior value.
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.attributes.insert(key.into(), value.into());
    }

    /// Set the final status. Default is `Unset` — only override if you
    /// have a reason (matches OTel convention).
    pub fn set_status(&mut self, status: SpanStatus) {
        self.status = status;
    }

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
    }

    /// The traceparent identifying this span (the in-process child of any
    /// inherited parent). Use for `TRACEPARENT` env export to children.
    pub fn traceparent(&self) -> &Traceparent {
        &self.traceparent
    }

    /// Encode as `TRACEPARENT=<...>` env pairs for a child process. Pairs
    /// rather than a single string so callers using `Command::envs` can
    /// extend without parsing.
    pub fn traceparent_env(&self) -> Vec<(String, String)> {
        vec![("TRACEPARENT".to_string(), self.traceparent.encode())]
    }

    /// Raw parent span id bytes — exposed mainly for assertion in tests.
    pub fn parent_span_id_bytes(&self) -> Vec<u8> {
        self.parent_span_id.clone()
    }

    /// Explicit close. Idempotent — Drop will not re-emit if this was
    /// already called. Useful when you want to set a final attribute that
    /// depends on the child's outcome and emit before the lexical scope ends.
    pub fn finish(mut self) {
        self.emit();
    }

    fn emit(&mut self) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        let end_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let attrs: Vec<KeyValue> = self
            .attributes
            .iter()
            .map(|(k, v)| KeyValue {
                key: k.clone(),
                value: Some(string_to_any_value(v)),
                ..Default::default()
            })
            .collect();

        let span = ProtoSpan {
            trace_id: self.traceparent.trace_id.to_vec(),
            span_id: self.traceparent.span_id.to_vec(),
            trace_state: String::new(),
            parent_span_id: self.parent_span_id.clone(),
            flags: 0,
            name: self.name.clone(),
            kind: 0,
            start_time_unix_nano: self.start_ns,
            end_time_unix_nano: end_ns,
            attributes: attrs,
            dropped_attributes_count: 0,
            events: Vec::new(),
            dropped_events_count: 0,
            links: Vec::new(),
            dropped_links_count: 0,
            status: Some(Status {
                message: if self.status == SpanStatus::Unset {
                    String::new()
                } else {
                    self.status_message.clone()
                },
                code: self.status.proto() as i32,
            }),
        };

        let rs = ResourceSpans {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_string(),
                    value: Some(AnyValue {
                        value: Some(AnyValueOneof::StringValue(
                            self.tracer.service_name().to_string(),
                        )),
                    }),
                    ..Default::default()
                }],
                dropped_attributes_count: 0,
                entity_refs: Vec::new(),
            }),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: "firestream-ci".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                spans: vec![span],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        };
        self.tracer.enqueue(rs);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // Drop must never panic — `emit` swallows errors. The bash spec
        // (`_close_on_exit`) is also non-fatal on emission failure.
        self.emit();
    }
}

fn string_to_any_value(v: &str) -> AnyValue {
    // Same probing order as `otel_cli::span::string_to_any_value`. Vendoring
    // the helper rather than re-exporting it: pulling another `pub use`
    // into firestream-ci's surface for one function would be more cost than copy.
    let value = if let Ok(i) = v.parse::<i64>() {
        AnyValueOneof::IntValue(i)
    } else if let Ok(f) = v.parse::<f64>() {
        AnyValueOneof::DoubleValue(f)
    } else {
        match v {
            "1" | "t" | "T" | "TRUE" | "true" | "True" => AnyValueOneof::BoolValue(true),
            "0" | "f" | "F" | "FALSE" | "false" | "False" => AnyValueOneof::BoolValue(false),
            _ => AnyValueOneof::StringValue(v.to_string()),
        }
    };
    AnyValue { value: Some(value) }
}
