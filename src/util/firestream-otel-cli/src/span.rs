//! Span building helpers.
//!
//! Go references:
//! - `otlpclient/protobuf_span.go` — protobuf span builders, kind/status enums,
//!   attribute conversion.
//! - `otelcli/config_span.go::NewProtobufSpan` — assembles a `Span` from a
//!   `Config`, honouring `force_*_id`, traceparent loading, and recording mode.
//!
//! We collapse those two layers into a single `build_resource_spans` entry
//! point: it takes a `Config` and emits a `ResourceSpans` wrapper (resource
//! attributes derived from `service_name`; everything else from the config).

use std::collections::BTreeMap;

use chrono::Utc;
use opentelemetry_proto::tonic::common::v1::{
    any_value::Value as AnyValueOneof, AnyValue, InstrumentationScope, KeyValue,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{
    span::SpanKind, status::StatusCode, ResourceSpans, ScopeSpans, Span, Status,
};
use rand::RngCore;

use crate::config::{parse_cli_time, Config};
use crate::traceparent::Traceparent;

/// Map otel-cli's span kind strings to the protobuf enum. Matches
/// `protobuf_span.go::SpanKindStringToInt`; unknown values map to
/// `SPAN_KIND_UNSPECIFIED`.
pub fn span_kind_from_str(kind: &str) -> SpanKind {
    match kind.to_ascii_lowercase().as_str() {
        "client" => SpanKind::Client,
        "server" => SpanKind::Server,
        "producer" => SpanKind::Producer,
        "consumer" => SpanKind::Consumer,
        "internal" => SpanKind::Internal,
        _ => SpanKind::Unspecified,
    }
}

/// Map otel-cli's status code strings to the protobuf enum. Mirrors
/// `protobuf_span.go::SpanStatusStringToInt` — unrecognised input maps to
/// `STATUS_CODE_UNSET`.
pub fn status_code_from_str(code: &str) -> StatusCode {
    match code.to_ascii_lowercase().as_str() {
        "ok" => StatusCode::Ok,
        "error" => StatusCode::Error,
        _ => StatusCode::Unset,
    }
}

/// Convert a `BTreeMap<String, String>` of CLI-supplied attributes into
/// `KeyValue` protobuf form. Mirrors `StringMapAttrsToProtobuf` — values are
/// probed against int / float / bool parsers in turn and only fall through to a
/// string when nothing matches.
pub fn attrs_to_keyvalues(attrs: &BTreeMap<String, String>) -> Vec<KeyValue> {
    attrs
        .iter()
        .map(|(k, v)| KeyValue {
            key: k.clone(),
            value: Some(string_to_any_value(v)),
            ..Default::default()
        })
        .collect()
}

/// Merge resource attributes into a span's attribute list. Span attrs win on
/// key collision because the caller explicitly set them for this span; the
/// resource attrs are process-wide defaults.
pub fn merge_resource_into_span_attrs(
    resource: &BTreeMap<String, String>,
    span: &BTreeMap<String, String>,
) -> Vec<KeyValue> {
    let mut merged: BTreeMap<&str, &str> =
        resource.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for (k, v) in span {
        merged.insert(k.as_str(), v.as_str());
    }
    merged
        .into_iter()
        .map(|(k, v)| KeyValue {
            key: k.to_string(),
            value: Some(string_to_any_value(v)),
            ..Default::default()
        })
        .collect()
}

/// Coerce a stringly-typed attribute value into the smallest matching
/// `AnyValue`. We probe int → float → bool → string in the same order as the Go
/// reference so round-tripping a span produces identical output.
pub fn string_to_any_value(v: &str) -> AnyValue {
    let value = if let Ok(i) = v.parse::<i64>() {
        AnyValueOneof::IntValue(i)
    } else if let Ok(f) = v.parse::<f64>() {
        AnyValueOneof::DoubleValue(f)
    } else if let Some(b) = parse_bool_str(v) {
        AnyValueOneof::BoolValue(b)
    } else {
        AnyValueOneof::StringValue(v.to_string())
    };
    AnyValue { value: Some(value) }
}

fn parse_bool_str(s: &str) -> Option<bool> {
    // Go's strconv.ParseBool accepts: 1, t, T, TRUE, true, True (and false variants).
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Build a complete `ResourceSpans` payload from `cfg`. The result has exactly
/// one resource (carrying `service.name`), one scope span, and one span.
///
/// This is the single entry point used by both the live `span` command and
/// future clients that build spans server-side (e.g. background mode).
pub fn build_resource_spans(cfg: &Config) -> ResourceSpans {
    let span = build_span(cfg);
    // Resource attributes: service.name plus anything from
    // OTEL_RESOURCE_ATTRIBUTES / cfg file. Caller-supplied resource attrs are
    // merged in *after* service.name so a caller can override it if they
    // really want to, matching SDK behaviour.
    let mut resource_attrs: Vec<KeyValue> = vec![KeyValue {
        key: "service.name".to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::StringValue(cfg.service_name.clone())),
        }),
        ..Default::default()
    }];
    for (k, v) in &cfg.resource_attributes {
        // Skip duplicate service.name keys; cfg.service_name is authoritative
        // unless the caller explicitly put service.name in the env var.
        if k == "service.name" {
            // Last-write-wins: replace the seeded entry.
            if let Some(existing) = resource_attrs.iter_mut().find(|kv| kv.key == "service.name") {
                existing.value = Some(string_to_any_value(v));
            }
            continue;
        }
        resource_attrs.push(KeyValue {
            key: k.clone(),
            value: Some(string_to_any_value(v)),
            ..Default::default()
        });
    }
    ResourceSpans {
        resource: Some(Resource {
            attributes: resource_attrs,
            dropped_attributes_count: 0,
            entity_refs: Vec::new(),
        }),
        scope_spans: vec![ScopeSpans {
            scope: Some(InstrumentationScope {
                name: "otel-cli".to_string(),
                version: cfg.version.clone(),
                attributes: Vec::new(),
                dropped_attributes_count: 0,
            }),
            spans: vec![span],
            schema_url: String::new(),
        }],
        schema_url: String::new(),
    }
}

/// Build the inner `Span` proto. Exposed for tests; production callers go
/// through `build_resource_spans`.
pub fn build_span(cfg: &Config) -> Span {
    let recording = cfg.is_recording();

    // Resolve identity. Default = empty (non-recording) or random (recording);
    // overridden by traceparent, finally overridden by `force_*_id` flags.
    let mut trace_id: Vec<u8> = if recording {
        random_bytes(16)
    } else {
        vec![0u8; 16]
    };
    let mut span_id: Vec<u8> = if recording {
        random_bytes(8)
    } else {
        vec![0u8; 8]
    };
    let mut parent_span_id: Vec<u8> = Vec::new();

    if recording {
        let tp = load_traceparent(cfg);
        if tp.initialized {
            trace_id = tp.trace_id.to_vec();
            parent_span_id = tp.span_id.to_vec();
        }
    }

    if !cfg.force_trace_id.is_empty() {
        if let Ok(b) = hex::decode(&cfg.force_trace_id) {
            if b.len() == 16 {
                trace_id = b;
            }
        }
    }
    if !cfg.force_span_id.is_empty() {
        if let Ok(b) = hex::decode(&cfg.force_span_id) {
            if b.len() == 8 {
                span_id = b;
            }
        }
    }
    if !cfg.force_parent_span_id.is_empty() {
        if let Ok(b) = hex::decode(&cfg.force_parent_span_id) {
            if b.len() == 8 {
                parent_span_id = b;
            }
        }
    }

    // Times: parse_cli_time on either side, falling back to now() on failure.
    let now = Utc::now();
    let start = parse_cli_time(&cfg.span_start_time).unwrap_or(now);
    let end = parse_cli_time(&cfg.span_end_time).unwrap_or(now);

    let start_ns = start.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;
    let end_ns = end.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;

    let status_code = status_code_from_str(&cfg.status_code);
    // Per OTel spec: only set the message when the code is Error/Ok; otherwise
    // leave it empty (the Go `SetSpanStatus` mirrors this for non-UNSET only).
    let status_message = if status_code == StatusCode::Unset {
        String::new()
    } else {
        cfg.status_description.clone()
    };

    Span {
        trace_id,
        span_id,
        trace_state: String::new(),
        parent_span_id,
        flags: 0,
        name: cfg.span_name.clone(),
        kind: span_kind_from_str(&cfg.kind) as i32,
        start_time_unix_nano: start_ns,
        end_time_unix_nano: end_ns,
        // Merge resource attrs into the span. The file-sink layout only writes
        // a Span (no Resource alongside), so without this copy the
        // OTEL_RESOURCE_ATTRIBUTES values vanish on disk. Span attrs override
        // resource attrs on key collision — caller intent is "I'm setting this
        // for this span specifically".
        attributes: merge_resource_into_span_attrs(
            &cfg.resource_attributes,
            &cfg.attributes,
        ),
        dropped_attributes_count: 0,
        events: Vec::new(),
        dropped_events_count: 0,
        links: Vec::new(),
        dropped_links_count: 0,
        status: Some(Status {
            message: status_message,
            code: status_code as i32,
        }),
    }
}

/// Encapsulates the "env first, file overrides" rule from
/// `config_span.go::LoadTraceparent`.
fn load_traceparent(cfg: &Config) -> Traceparent {
    let mut tp = Traceparent::default();

    if !cfg.traceparent_ignore_env {
        if let Some(Ok(parsed)) = Traceparent::from_env() {
            tp = parsed;
        }
    }

    if !cfg.traceparent_carrier_file.is_empty() {
        if let Ok(file_tp) = Traceparent::from_file(&cfg.traceparent_carrier_file) {
            if file_tp.initialized {
                tp = file_tp;
            }
        }
    }

    tp
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn recording_config() -> Config {
        let mut c = Config::defaults();
        c.endpoint = "http://example:4317".to_string();
        c.span_name = "test-span".to_string();
        c.kind = "server".to_string();
        c
    }

    #[test]
    fn span_kind_parses() {
        assert_eq!(span_kind_from_str("client"), SpanKind::Client);
        assert_eq!(span_kind_from_str("SERVER"), SpanKind::Server);
        assert_eq!(span_kind_from_str("producer"), SpanKind::Producer);
        assert_eq!(span_kind_from_str("consumer"), SpanKind::Consumer);
        assert_eq!(span_kind_from_str("internal"), SpanKind::Internal);
        assert_eq!(span_kind_from_str("garbage"), SpanKind::Unspecified);
    }

    #[test]
    fn status_code_parses() {
        assert_eq!(status_code_from_str("unset"), StatusCode::Unset);
        assert_eq!(status_code_from_str("ok"), StatusCode::Ok);
        assert_eq!(status_code_from_str("ERROR"), StatusCode::Error);
        assert_eq!(status_code_from_str("garbage"), StatusCode::Unset);
    }

    #[test]
    fn string_to_any_value_int_first() {
        let v = string_to_any_value("42");
        match v.value.unwrap() {
            AnyValueOneof::IntValue(i) => assert_eq!(i, 42),
            other => panic!("expected IntValue, got {other:?}"),
        }
    }

    #[test]
    fn string_to_any_value_float_when_not_int() {
        // Use a value that isn't an approximation of a math constant to keep
        // clippy::approx_constant happy.
        let v = string_to_any_value("1.5");
        match v.value.unwrap() {
            AnyValueOneof::DoubleValue(f) => assert!((f - 1.5).abs() < 1e-9),
            other => panic!("expected DoubleValue, got {other:?}"),
        }
    }

    #[test]
    fn string_to_any_value_bool_when_not_numeric() {
        let v = string_to_any_value("true");
        match v.value.unwrap() {
            AnyValueOneof::BoolValue(b) => assert!(b),
            other => panic!("expected BoolValue, got {other:?}"),
        }
    }

    #[test]
    fn string_to_any_value_string_fallback() {
        let v = string_to_any_value("hello world");
        match v.value.unwrap() {
            AnyValueOneof::StringValue(s) => assert_eq!(s, "hello world"),
            other => panic!("expected StringValue, got {other:?}"),
        }
    }

    #[test]
    fn build_span_basic() {
        let mut cfg = recording_config();
        cfg.attributes.insert("k1".to_string(), "v1".to_string());
        cfg.attributes.insert("count".to_string(), "7".to_string());

        let rs = build_resource_spans(&cfg);

        // Resource carries service.name
        let resource = rs.resource.as_ref().expect("resource present");
        let svc = resource
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .expect("service.name kv");
        match svc.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::StringValue(s) => assert_eq!(s, "otel-cli"),
            other => panic!("unexpected: {other:?}"),
        }

        // Exactly one span with the expected name/kind/attrs
        let span = &rs.scope_spans[0].spans[0];
        assert_eq!(span.name, "test-span");
        assert_eq!(span.kind, SpanKind::Server as i32);
        assert_eq!(span.attributes.len(), 2);

        // Recording → trace/span ids are 16/8 bytes and non-zero
        assert_eq!(span.trace_id.len(), 16);
        assert_eq!(span.span_id.len(), 8);
        assert!(span.trace_id.iter().any(|b| *b != 0));
        assert!(span.span_id.iter().any(|b| *b != 0));
    }

    #[test]
    fn build_span_force_ids() {
        let mut cfg = recording_config();
        cfg.force_trace_id = "0af7651916cd43dd8448eb211c80319c".to_string();
        cfg.force_span_id = "b7ad6b7169203331".to_string();
        cfg.force_parent_span_id = "aaaaaaaaaaaaaaaa".to_string();

        let rs = build_resource_spans(&cfg);
        let span = &rs.scope_spans[0].spans[0];

        assert_eq!(hex::encode(&span.trace_id), "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(hex::encode(&span.span_id), "b7ad6b7169203331");
        assert_eq!(hex::encode(&span.parent_span_id), "aaaaaaaaaaaaaaaa");
    }

    /// When a traceparent is loaded from the carrier file the span should
    /// inherit its trace id and treat its span id as the parent. Force-ids
    /// then take precedence; here we test without force-ids.
    #[test]
    fn build_span_from_traceparent() {
        let mut cfg = recording_config();
        // Disable env so the test isn't influenced by an outer TRACEPARENT.
        cfg.traceparent_ignore_env = true;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let tp =
            Traceparent::parse("00-11112222333344445555666677778888-aabbccddeeff0011-01").unwrap();
        tp.write_to_file(tmp.path(), false).unwrap();
        cfg.traceparent_carrier_file = tmp.path().display().to_string();

        let rs = build_resource_spans(&cfg);
        let span = &rs.scope_spans[0].spans[0];

        assert_eq!(hex::encode(&span.trace_id), "11112222333344445555666677778888");
        assert_eq!(hex::encode(&span.parent_span_id), "aabbccddeeff0011");
        // span id is fresh (random) so it must not equal the parent
        assert_ne!(hex::encode(&span.span_id), "aabbccddeeff0011");
    }

    #[test]
    fn build_span_non_recording_uses_zero_ids() {
        // Default config has no endpoint → non-recording → zero ids.
        let cfg = Config::defaults();
        let span = build_span(&cfg);
        assert!(span.trace_id.iter().all(|b| *b == 0));
        assert!(span.span_id.iter().all(|b| *b == 0));
    }
}
