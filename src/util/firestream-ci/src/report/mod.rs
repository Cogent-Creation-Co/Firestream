//! Pattern #19 from the plan: span tree → phase table / build table /
//! markdown summary. Mirrors `_otel.sh:432-675`.
//!
//! Reads `span.json` files produced by the JSON-file OTLP sink (or by the
//! checkpoint-replay path) and assembles a typed [`Report`] that can render
//! markdown tables comparable to the bash summary output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use comfy_table::{ContentArrangement, Table};
use opentelemetry_proto::tonic::trace::v1::Span as ProtoSpan;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("report: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("report: parse error in `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// One span entry with the bits we need for the summary tables.
#[derive(Debug, Clone)]
pub struct SpanEntry {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    pub duration: Duration,
    /// 0 = Unset, 1 = Ok, 2 = Error (mirrors OTel StatusCode).
    pub status_code: i32,
    pub status_message: String,
    /// nix.derivation.name attribute, when present. Used to identify build
    /// artifacts in the build table.
    pub derivation_name: Option<String>,
    /// pipeline.phase / phase attribute, when present.
    pub phase: Option<String>,
}

/// A scanned span tree.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub spans: Vec<SpanEntry>,
}

impl Report {
    /// Walk a span directory with the standard layout
    /// `<dir>/<traceHex>/<spanHex>/span.json` and load every span we find.
    pub fn from_span_dir(dir: &Path) -> Result<Self, Error> {
        let mut spans = Vec::new();
        if !dir.is_dir() {
            return Ok(Self { spans });
        }
        for trace_entry in walkdir::WalkDir::new(dir)
            .max_depth(4)
            .into_iter()
            .flatten()
        {
            if !trace_entry.file_type().is_file() {
                continue;
            }
            if trace_entry.file_name() != "span.json" {
                continue;
            }
            let path = trace_entry.path();
            let bytes = std::fs::read(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let span: ProtoSpan = match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(source) => {
                    return Err(Error::Parse {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            };
            spans.push(span_to_entry(&span));
        }
        // Stable order: by start time isn't available without scanning the
        // proto field; fall back to (trace_id, span_id) which is reproducible.
        spans.sort_by(|a, b| {
            (a.trace_id.as_str(), a.span_id.as_str())
                .cmp(&(b.trace_id.as_str(), b.span_id.as_str()))
        });
        Ok(Self { spans })
    }

    /// Markdown table of phase × outcome × duration. "Phases" here means any
    /// span whose `phase` attribute is set (the rest of the spans surface in
    /// `build_table`).
    pub fn phase_table(&self) -> String {
        let mut t = Table::new();
        t.set_content_arrangement(ContentArrangement::Dynamic);
        t.set_header(vec!["Phase", "Outcome", "Duration"]);
        for s in &self.spans {
            let Some(phase) = s.phase.as_deref() else {
                continue;
            };
            t.add_row(vec![
                phase.to_string(),
                outcome_label(s.status_code).to_string(),
                format!("{:?}", s.duration),
            ]);
        }
        format!("{t}")
    }

    /// Markdown table of build artifacts. Includes every span with a
    /// `nix.derivation.name` attribute.
    pub fn build_table(&self) -> String {
        let mut t = Table::new();
        t.set_content_arrangement(ContentArrangement::Dynamic);
        t.set_header(vec!["Artifact", "Outcome", "Duration", "Message"]);
        for s in &self.spans {
            let Some(name) = s.derivation_name.as_deref() else {
                continue;
            };
            t.add_row(vec![
                name.to_string(),
                outcome_label(s.status_code).to_string(),
                format!("{:?}", s.duration),
                s.status_message.clone(),
            ]);
        }
        format!("{t}")
    }

    /// Write the report as newline-delimited JSON, one record per
    /// [`SpanEntry`]. The on-disk shape is decoupled from the in-memory
    /// struct via a private DTO so future fields can be added to
    /// [`SpanEntry`] without breaking downstream consumers — and so
    /// `Duration` serialises as an explicit `duration_ns: u64` rather than
    /// serde's default tuple-or-struct shape.
    ///
    /// Writes are atomic: the body is staged at `<path>.tmp` and then
    /// renamed over `path` on success. Each record is newline-terminated,
    /// including the final one, so external tools can use line-oriented
    /// readers without a trailing-empty-line special case.
    pub fn write_ndjson(&self, path: &Path) -> Result<(), Error> {
        let tmp = path.with_extension("ndjson.tmp");
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let mut body = String::new();
        for entry in &self.spans {
            let dto = SpanEntryDto::from(entry);
            let line = serde_json::to_string(&dto).map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })?;
            body.push_str(&line);
            body.push('\n');
        }
        std::fs::write(&tmp, &body).map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Stable JSON representation of the report, suitable for piping to
    /// `jq` or feeding to Honeycomb's trace explorer. Wraps the same
    /// per-span DTO used by [`Report::write_ndjson`] under a versioned
    /// envelope: NDJSON has no wrapper (one record per line, version
    /// implicit in the file shape), but the array form needs a wrapper
    /// object to carry the version, so we hard-code `schema_version: 1`
    /// here. Bumps come later if the per-span DTO changes incompatibly.
    pub fn to_json(&self) -> serde_json::Value {
        let dtos: Vec<SpanEntryDto<'_>> = self.spans.iter().map(SpanEntryDto::from).collect();
        serde_json::json!({
            "schema_version": 1,
            "spans": dtos,
        })
    }

    /// Atomically write [`Self::to_json`] to `path` as pretty-printed
    /// JSON. Mirrors [`Self::write_ndjson`]'s staging-then-rename pattern
    /// so partial writes never replace a good file.
    pub fn write_json(&self, path: &Path) -> Result<(), Error> {
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let value = self.to_json();
        let body = serde_json::to_string_pretty(&value).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(&tmp, &body).map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Write a markdown summary file mirroring the bash `_otel.sh` shape:
    /// a header, the phase table, then the build table.
    pub fn summary_markdown(&self, path: &Path) -> Result<(), Error> {
        let mut out = String::new();
        out.push_str("# OTel Summary\n\n## Phases\n\n");
        out.push_str(&self.phase_table());
        out.push_str("\n\n## Builds\n\n");
        out.push_str(&self.build_table());
        out.push('\n');
        std::fs::write(path, out).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// On-disk shape for `Report::write_ndjson`. Decoupled from
/// [`SpanEntry`] so [`Duration`] serialises as a flat `duration_ns: u64`
/// — serde's default for `Duration` is `{secs, nanos}`, which is awkward
/// to query with `jq`. Keep this struct private; downstream consumers
/// should treat the NDJSON file as the API.
#[derive(Serialize)]
struct SpanEntryDto<'a> {
    name: &'a str,
    trace_id: &'a str,
    span_id: &'a str,
    parent_span_id: &'a str,
    duration_ns: u64,
    status_code: i32,
    status_message: &'a str,
    phase: Option<&'a str>,
    derivation_name: Option<&'a str>,
}

impl<'a> From<&'a SpanEntry> for SpanEntryDto<'a> {
    fn from(s: &'a SpanEntry) -> Self {
        // `Duration::as_nanos()` returns u128. Saturating into u64 keeps the
        // on-disk type simple; ~584 years of nanoseconds is more than any
        // CI run will ever reach, so the saturation arm is unreachable in
        // practice.
        let duration_ns = s.duration.as_nanos().min(u64::MAX as u128) as u64;
        Self {
            name: &s.name,
            trace_id: &s.trace_id,
            span_id: &s.span_id,
            parent_span_id: &s.parent_span_id,
            duration_ns,
            status_code: s.status_code,
            status_message: &s.status_message,
            phase: s.phase.as_deref(),
            derivation_name: s.derivation_name.as_deref(),
        }
    }
}

fn outcome_label(code: i32) -> &'static str {
    match code {
        1 => "passed",
        2 => "failed",
        _ => "unset",
    }
}

fn span_to_entry(span: &ProtoSpan) -> SpanEntry {
    use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyVal;

    let mut derivation_name: Option<String> = None;
    let mut phase: Option<String> = None;
    for attr in &span.attributes {
        let Some(v) = attr.value.as_ref().and_then(|v| v.value.as_ref()) else {
            continue;
        };
        let val_str = match v {
            AnyVal::StringValue(s) => Some(s.clone()),
            _ => None,
        };
        match attr.key.as_str() {
            "nix.derivation.name" => derivation_name = val_str,
            "phase" | "pipeline.phase" => {
                if phase.is_none() {
                    phase = val_str;
                }
            }
            _ => {}
        }
    }

    let duration = Duration::from_nanos(
        span.end_time_unix_nano
            .saturating_sub(span.start_time_unix_nano),
    );
    let (status_code, status_message) = match span.status.as_ref() {
        Some(st) => (st.code, st.message.clone()),
        None => (0, String::new()),
    };

    SpanEntry {
        name: span.name.clone(),
        trace_id: hex::encode(&span.trace_id),
        span_id: hex::encode(&span.span_id),
        parent_span_id: hex::encode(&span.parent_span_id),
        duration,
        status_code,
        status_message,
        derivation_name,
        phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value::Value as AnyVal};
    use opentelemetry_proto::tonic::trace::v1::Status;

    fn write_span(dir: &Path, trace: &str, span: &str, p: ProtoSpan) {
        let d = dir.join(trace).join(span);
        std::fs::create_dir_all(&d).unwrap();
        let bytes = serde_json::to_vec_pretty(&p).unwrap();
        std::fs::write(d.join("span.json"), bytes).unwrap();
    }

    fn span_with_attrs(name: &str, attrs: Vec<KeyValue>, code: i32) -> ProtoSpan {
        ProtoSpan {
            trace_id: vec![1; 16],
            span_id: vec![2; 8],
            parent_span_id: vec![],
            name: name.into(),
            attributes: attrs,
            status: Some(Status {
                message: String::new(),
                code,
            }),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000_000,
            ..Default::default()
        }
    }

    fn kv(k: &str, v: &str) -> KeyValue {
        KeyValue {
            key: k.into(),
            value: Some(AnyValue {
                value: Some(AnyVal::StringValue(v.into())),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn empty_dir_yields_empty_report() {
        let tmp = tempfile::tempdir().unwrap();
        let r = Report::from_span_dir(tmp.path()).unwrap();
        assert!(r.spans.is_empty());
    }

    #[test]
    fn loads_span_and_extracts_derivation_name() {
        let tmp = tempfile::tempdir().unwrap();
        let s = span_with_attrs(
            "nix.build",
            vec![kv("nix.derivation.name", "demo-server-0.1.0")],
            1,
        );
        write_span(tmp.path(), "aa", "bb", s);
        let r = Report::from_span_dir(tmp.path()).unwrap();
        assert_eq!(r.spans.len(), 1);
        assert_eq!(
            r.spans[0].derivation_name.as_deref(),
            Some("demo-server-0.1.0")
        );
        assert_eq!(r.spans[0].status_code, 1);
    }

    #[test]
    fn phase_table_lists_phase_attr_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let s = span_with_attrs("phase.one", vec![kv("phase", "phase-1")], 1);
        write_span(tmp.path(), "aa", "bb", s);
        let r = Report::from_span_dir(tmp.path()).unwrap();
        let table = r.phase_table();
        assert!(table.contains("phase-1"), "table:\n{table}");
        assert!(table.contains("passed"), "table:\n{table}");
    }

    #[test]
    fn build_table_lists_derivation_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let s = span_with_attrs("nix.build", vec![kv("nix.derivation.name", "foo-1.0")], 2);
        write_span(tmp.path(), "aa", "bb", s);
        let r = Report::from_span_dir(tmp.path()).unwrap();
        let table = r.build_table();
        assert!(table.contains("foo-1.0"));
        assert!(table.contains("failed"));
    }

    /// Two synthetic spans in the standard `<trace>/<span>/span.json`
    /// layout → `Report::from_span_dir` → `write_ndjson` → read back
    /// line-by-line. Asserts: line count matches input, every line parses
    /// as JSON, `duration_ns` is an unsigned integer, `name` matches.
    #[test]
    fn write_ndjson_round_trips_each_line() {
        let tmp = tempfile::tempdir().unwrap();
        let spans_dir = tmp.path().join("spans");
        std::fs::create_dir_all(&spans_dir).unwrap();

        // Two spans under distinct (trace, span) ids — `from_span_dir` sorts
        // by (trace_id, span_id) so the read-back order is deterministic.
        let mut s1 = span_with_attrs("nix.build.one", vec![kv("phase", "verify")], 1);
        s1.trace_id = vec![0xaa; 16];
        s1.span_id = vec![0x11; 8];
        s1.end_time_unix_nano = 2_500_000_000;
        let trace_hex_1 = hex::encode(&s1.trace_id);
        let span_hex_1 = hex::encode(&s1.span_id);
        write_span(&spans_dir, &trace_hex_1, &span_hex_1, s1);

        let mut s2 = span_with_attrs(
            "nix.build.two",
            vec![kv("nix.derivation.name", "foo-1.0")],
            2,
        );
        s2.trace_id = vec![0xbb; 16];
        s2.span_id = vec![0x22; 8];
        s2.end_time_unix_nano = 750_000_000;
        let trace_hex_2 = hex::encode(&s2.trace_id);
        let span_hex_2 = hex::encode(&s2.span_id);
        write_span(&spans_dir, &trace_hex_2, &span_hex_2, s2);

        let report = Report::from_span_dir(&spans_dir).unwrap();
        assert_eq!(report.spans.len(), 2);

        let out = tmp.path().join("profiles").join("spans.ndjson");
        report.write_ndjson(&out).unwrap();

        let body = std::fs::read_to_string(&out).unwrap();
        // Every record newline-terminated, including the last one. So the
        // final `lines()` count equals the record count.
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "body:\n{body}");
        assert!(
            body.ends_with('\n'),
            "ndjson must terminate every record with a newline"
        );

        let mut names: Vec<String> = Vec::new();
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("ndjson line is JSON");
            // duration_ns must be a u64-shaped number (positive integer).
            let dur = v
                .get("duration_ns")
                .and_then(|x| x.as_u64())
                .expect("duration_ns is u64");
            assert!(dur > 0, "synthetic spans have nonzero duration; got {dur}");
            names.push(
                v.get("name")
                    .and_then(|x| x.as_str())
                    .expect("name field")
                    .to_string(),
            );
        }
        // sort-by-(trace_id, span_id): trace_hex_1 (`aa…`) < trace_hex_2 (`bb…`).
        assert_eq!(names, vec!["nix.build.one", "nix.build.two"]);
    }

    /// Build a `Report` directly (no on-disk spans) so the test stays
    /// hermetic — `to_json` is just a serialisation method, no I/O.
    /// Asserts the wrapper shape, schema version, the explicit field set
    /// per entry, and that `duration_ns` deserialises as `u64`.
    #[test]
    fn to_json_is_stable_and_versioned() {
        let report = Report {
            spans: vec![
                SpanEntry {
                    name: "phase.one".into(),
                    trace_id: "aa".repeat(16),
                    span_id: "11".repeat(8),
                    parent_span_id: String::new(),
                    duration: Duration::from_nanos(2_500_000_000),
                    status_code: 1,
                    status_message: String::new(),
                    derivation_name: None,
                    phase: Some("verify".into()),
                },
                SpanEntry {
                    name: "nix.build".into(),
                    trace_id: "bb".repeat(16),
                    span_id: "22".repeat(8),
                    parent_span_id: "33".repeat(8),
                    duration: Duration::from_nanos(750_000_000),
                    status_code: 2,
                    status_message: "boom".into(),
                    derivation_name: Some("foo-1.0".into()),
                    phase: None,
                },
            ],
        };

        let v = report.to_json();

        assert_eq!(
            v.get("schema_version").and_then(|x| x.as_u64()),
            Some(1),
            "schema_version must be present and equal to 1; got {v}"
        );
        let spans = v
            .get("spans")
            .and_then(|x| x.as_array())
            .expect("spans array");
        assert_eq!(spans.len(), 2);

        // Explicit field set per the on-disk contract. If any of these
        // names change, downstream `jq` queries break — the test is the
        // dam.
        let expected_keys = [
            "name",
            "trace_id",
            "span_id",
            "parent_span_id",
            "duration_ns",
            "status_code",
            "status_message",
            "phase",
            "derivation_name",
        ];
        for entry in spans {
            let obj = entry.as_object().expect("entry is object");
            for k in &expected_keys {
                assert!(obj.contains_key(*k), "entry missing key {k}: {entry}");
            }
            // duration_ns must serialise as an unsigned integer (the whole
            // point of the on-disk DTO; raw `Duration` would be a struct).
            let dur = obj
                .get("duration_ns")
                .and_then(|x| x.as_u64())
                .expect("duration_ns is u64");
            assert!(dur > 0, "synthetic spans have nonzero duration; got {dur}");
        }

        // Entry-level sanity: first span has `phase: "verify"` and no
        // derivation; second is the inverse.
        assert_eq!(
            spans[0].get("phase").and_then(|x| x.as_str()),
            Some("verify")
        );
        assert!(
            spans[0]
                .get("derivation_name")
                .map(|x| x.is_null())
                .unwrap_or(false)
        );
        assert_eq!(
            spans[1].get("derivation_name").and_then(|x| x.as_str()),
            Some("foo-1.0")
        );
        assert!(spans[1].get("phase").map(|x| x.is_null()).unwrap_or(false));
    }

    /// `write_json` round-trips: write to a tempfile, read back via
    /// `serde_json::from_str`, assert the wrapper + span count.
    #[test]
    fn write_json_round_trips_via_serde_json() {
        let tmp = tempfile::tempdir().unwrap();
        let report = Report {
            spans: vec![SpanEntry {
                name: "only".into(),
                trace_id: "aa".repeat(16),
                span_id: "11".repeat(8),
                parent_span_id: String::new(),
                duration: Duration::from_nanos(42),
                status_code: 1,
                status_message: String::new(),
                derivation_name: None,
                phase: Some("p".into()),
            }],
        };
        let out = tmp.path().join("profiles").join("spans.json");
        report.write_json(&out).unwrap();

        let body = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).expect("written file is valid JSON");
        assert_eq!(v.get("schema_version").and_then(|x| x.as_u64()), Some(1));
        let spans = v.get("spans").and_then(|x| x.as_array()).expect("spans");
        assert_eq!(spans.len(), report.spans.len());
        assert_eq!(spans[0].get("name").and_then(|x| x.as_str()), Some("only"));
        // duration_ns is the explicit nanos field, not Duration's default
        // serde shape (which would be `{secs, nanos}`).
        assert_eq!(
            spans[0].get("duration_ns").and_then(|x| x.as_u64()),
            Some(42)
        );
    }

    /// JSON and NDJSON share the per-span DTO; this asserts that
    /// invariant by writing both from the same `Report` and matching the
    /// `(trace_id, span_id)` set.
    #[test]
    fn to_json_and_write_ndjson_describe_same_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let report = Report {
            spans: vec![
                SpanEntry {
                    name: "a".into(),
                    trace_id: "aa".repeat(16),
                    span_id: "11".repeat(8),
                    parent_span_id: String::new(),
                    duration: Duration::from_nanos(1),
                    status_code: 1,
                    status_message: String::new(),
                    derivation_name: None,
                    phase: None,
                },
                SpanEntry {
                    name: "b".into(),
                    trace_id: "bb".repeat(16),
                    span_id: "22".repeat(8),
                    parent_span_id: String::new(),
                    duration: Duration::from_nanos(2),
                    status_code: 1,
                    status_message: String::new(),
                    derivation_name: None,
                    phase: None,
                },
            ],
        };

        let json_path = tmp.path().join("spans.json");
        let ndjson_path = tmp.path().join("spans.ndjson");
        report.write_json(&json_path).unwrap();
        report.write_ndjson(&ndjson_path).unwrap();

        let json_body = std::fs::read_to_string(&json_path).unwrap();
        let json_val: serde_json::Value = serde_json::from_str(&json_body).unwrap();
        let json_spans = json_val.get("spans").and_then(|x| x.as_array()).unwrap();

        let ndjson_body = std::fs::read_to_string(&ndjson_path).unwrap();
        let ndjson_spans: Vec<serde_json::Value> = ndjson_body
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect();

        assert_eq!(json_spans.len(), ndjson_spans.len());
        assert_eq!(json_spans.len(), report.spans.len());

        let key = |v: &serde_json::Value| -> (String, String) {
            (
                v.get("trace_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                v.get("span_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        };
        let mut json_keys: Vec<_> = json_spans.iter().map(key).collect();
        let mut nd_keys: Vec<_> = ndjson_spans.iter().map(key).collect();
        json_keys.sort();
        nd_keys.sort();
        assert_eq!(
            json_keys, nd_keys,
            "json and ndjson must describe the same spans"
        );
    }

    #[test]
    fn summary_markdown_writes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let r = Report { spans: vec![] };
        let path = tmp.path().join("summary.md");
        r.summary_markdown(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# OTel Summary"));
        assert!(text.contains("## Phases"));
        assert!(text.contains("## Builds"));
    }
}
