//! `otel-cli reconcile-spans` — override per-derivation span status using
//! `nix-fast-build`'s authoritative `--result-file` JSON.
//!
//! Background: `nix-ingest` derives per-span pass/fail from a stderr-line
//! heuristic (no per-derivation failure record exists in Nix's internal-json).
//! That heuristic can both false-positive (English "failed"/"error" tokens in
//! benign output) and false-negative (build phase that exits non-zero without
//! emitting an anchored failure prefix). `nix-fast-build` does know the
//! verdict and writes it to `--result-file <json>` in shape
//! `{"results": [{"attr", "success", "outputs": {"out": "/nix/store/HASH-NAME"}, "type"}]}`.
//!
//! This subcommand reads that JSON, derives each entry's derivation name from
//! its output store path (stem after the 32-char hash + dash), walks every
//! span.json under `--spans-dir`, and for any span whose
//! `nix.derivation.name` attribute matches a result entry, rewrites
//! `status.code` to 1 (OK) or 2 (Error) per the JSON verdict.
//!
//! Spans without a matching JSON entry are left alone — for those, the
//! heuristic-derived status remains the best signal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use opentelemetry_proto::tonic::trace::v1::Span;
use serde::Deserialize;
use thiserror::Error;
use tokio::fs;

#[derive(Debug, Args)]
pub struct ReconcileArgs {
    /// Path to a nix-fast-build `--result-file` JSON file.
    #[arg(long = "result-file")]
    pub result_file: PathBuf,

    /// Span directory root (`<dir>/<traceHex>/<spanHex>/span.json` layout,
    /// matching `cli::nix_ingest` output via the `json+file` client).
    /// Falls back to `OTEL_SPAN_DIR`.
    #[arg(long = "spans-dir", env = "OTEL_SPAN_DIR")]
    pub spans_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ResultFile {
    #[serde(default)]
    results: Vec<ResultEntry>,
}

#[derive(Debug, Deserialize)]
struct ResultEntry {
    #[serde(default)]
    success: bool,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    outputs: Option<HashMap<String, String>>,
    // `attr` is currently emitted as an empty string by firestream-nix-build's
    // serializer, so we intentionally don't depend on it.
}

/// Typed error returned by the library entry point [`reconcile_spans`].
///
/// The CLI subcommand catches these and turns them into `anyhow::Error` for
/// uniform error reporting; downstream library callers get a discriminated
/// error so they can react to (e.g.) a missing result file without parsing
/// error strings.
#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("read result file {path:?}: {source}")]
    ReadResultFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse result file {path:?}: {source}")]
    ParseResultFile {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("walk spans dir {path:?}: {source}")]
    WalkSpans {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("rewrite span at {path:?}: {source}")]
    RewriteSpan {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serialize span at {path:?}: {source}")]
    SerializeSpan {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Aggregate counts returned by [`reconcile_spans`].
///
/// `skipped_no_verdicts` is set when the result file parses cleanly but
/// contains zero BUILD entries with usable outputs — there's nothing to
/// reconcile, and the walk over `spans_dir` is skipped entirely.
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// True when the result file held no BUILD entries with outputs; the
    /// walk was skipped and the other counters are all zero.
    pub skipped_no_verdicts: bool,
    /// Total number of `span.json` files inspected.
    pub scanned: u32,
    /// Number of `span.json` files whose status was rewritten.
    pub updated: u32,
    /// Number of distinct derivation-name → success verdicts parsed from
    /// the result file.
    pub verdicts: u32,
}

/// Library entry point — reconcile span statuses against a nix-fast-build
/// result file.
///
/// Reads `result_file`, derives a `derivation-name → success` map from the
/// BUILD entries, walks `spans_dir` for `span.json` files, and rewrites the
/// `status.code` of any span whose `nix.derivation.name` attribute matches an
/// entry. Spans without a matching verdict are left untouched.
///
/// `output_dir` is reserved for a future mode that mirrors rewritten spans to
/// a separate tree. The current implementation always rewrites in place under
/// `spans_dir`; callers that pass `output_dir != spans_dir` should treat that
/// as a no-op for now. Documenting it here pins the signature against future
/// expansion without breaking the surface area.
pub async fn reconcile_spans(
    result_file: &Path,
    spans_dir: &Path,
    _output_dir: &Path,
) -> Result<ReconcileReport, ReconcileError> {
    let bytes = fs::read(result_file)
        .await
        .map_err(|source| ReconcileError::ReadResultFile {
            path: result_file.to_path_buf(),
            source,
        })?;
    let parsed: ResultFile =
        serde_json::from_slice(&bytes).map_err(|source| ReconcileError::ParseResultFile {
            path: result_file.to_path_buf(),
            source,
        })?;

    let mut verdicts: HashMap<String, bool> = HashMap::new();
    for entry in &parsed.results {
        if !entry.kind.eq_ignore_ascii_case("BUILD") {
            continue;
        }
        if let Some(name) = derivation_name_from_outputs(&entry.outputs) {
            verdicts.insert(name, entry.success);
        }
    }

    if verdicts.is_empty() {
        return Ok(ReconcileReport {
            skipped_no_verdicts: true,
            scanned: 0,
            updated: 0,
            verdicts: 0,
        });
    }

    let (scanned, updated) = walk_and_reconcile(spans_dir, &verdicts).await?;
    Ok(ReconcileReport {
        skipped_no_verdicts: false,
        scanned,
        updated,
        verdicts: verdicts.len() as u32,
    })
}

pub async fn run(args: ReconcileArgs) -> Result<u8> {
    let report = reconcile_spans(&args.result_file, &args.spans_dir, &args.spans_dir)
        .await
        .with_context(|| {
            format!(
                "reconcile-spans (result-file={}, spans-dir={})",
                args.result_file.display(),
                args.spans_dir.display()
            )
        })?;

    if report.skipped_no_verdicts {
        eprintln!(
            "otel-cli reconcile-spans: no BUILD entries with outputs in {} — nothing to reconcile",
            args.result_file.display()
        );
        return Ok(0);
    }

    eprintln!(
        "otel-cli reconcile-spans: scanned={} updated={} verdicts={} result-file={}",
        report.scanned,
        report.updated,
        report.verdicts,
        args.result_file.display()
    );
    Ok(0)
}

async fn walk_and_reconcile(
    spans_dir: &Path,
    verdicts: &HashMap<String, bool>,
) -> Result<(u32, u32), ReconcileError> {
    let mut scanned = 0u32;
    let mut updated = 0u32;

    let mut trace_entries = match fs::read_dir(spans_dir).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(source) => {
            return Err(ReconcileError::WalkSpans {
                path: spans_dir.to_path_buf(),
                source,
            });
        }
    };
    loop {
        let trace_entry = trace_entries
            .next_entry()
            .await
            .map_err(|source| ReconcileError::WalkSpans {
                path: spans_dir.to_path_buf(),
                source,
            })?;
        let Some(trace_entry) = trace_entry else {
            break;
        };
        let trace_path = trace_entry.path();
        if !trace_path.is_dir() {
            continue;
        }
        let mut span_entries =
            fs::read_dir(&trace_path)
                .await
                .map_err(|source| ReconcileError::WalkSpans {
                    path: trace_path.clone(),
                    source,
                })?;
        loop {
            let span_entry =
                span_entries
                    .next_entry()
                    .await
                    .map_err(|source| ReconcileError::WalkSpans {
                        path: trace_path.clone(),
                        source,
                    })?;
            let Some(span_entry) = span_entry else {
                break;
            };
            let span_path = span_entry.path().join("span.json");
            if !span_path.is_file() {
                continue;
            }
            scanned += 1;
            if reconcile_one(&span_path, verdicts).await? {
                updated += 1;
            }
        }
    }
    Ok((scanned, updated))
}

/// Returns Ok(true) if the span file was rewritten, Ok(false) if no change.
async fn reconcile_one(
    span_path: &Path,
    verdicts: &HashMap<String, bool>,
) -> Result<bool, ReconcileError> {
    let bytes = fs::read(span_path)
        .await
        .map_err(|source| ReconcileError::RewriteSpan {
            path: span_path.to_path_buf(),
            source,
        })?;
    let mut span: Span = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(false), // not a span.json we can parse; skip
    };

    let name = match span_derivation_name(&span) {
        Some(n) => n,
        None => return Ok(false),
    };
    let Some(&success) = verdicts.get(&name) else {
        return Ok(false);
    };

    let want_code: i32 = if success { 1 } else { 2 }; // StatusCode: Ok=1, Error=2
    let want_msg = if success {
        ""
    } else {
        "nix-fast-build reported failure"
    };
    let cur_code = span.status.as_ref().map(|s| s.code).unwrap_or(0);
    if cur_code == want_code {
        return Ok(false);
    }

    span.status = Some(opentelemetry_proto::tonic::trace::v1::Status {
        message: want_msg.to_string(),
        code: want_code,
    });
    let new_bytes = serde_json::to_vec_pretty(&span).map_err(|source| {
        ReconcileError::SerializeSpan {
            path: span_path.to_path_buf(),
            source,
        }
    })?;
    fs::write(span_path, new_bytes)
        .await
        .map_err(|source| ReconcileError::RewriteSpan {
            path: span_path.to_path_buf(),
            source,
        })?;
    Ok(true)
}

fn span_derivation_name(span: &Span) -> Option<String> {
    use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValueOneof;
    span.attributes.iter().find_map(|kv| {
        if kv.key != "nix.derivation.name" {
            return None;
        }
        let v = kv.value.as_ref()?;
        match v.value.as_ref()? {
            AnyValueOneof::StringValue(s) => Some(s.clone()),
            _ => None,
        }
    })
}

/// `/nix/store/<32-char hash>-<name>` → `<name>`.
/// Returns None if the path doesn't look like a Nix store output.
fn derivation_name_from_outputs(outputs: &Option<HashMap<String, String>>) -> Option<String> {
    let outs = outputs.as_ref()?;
    // Prefer "out", fall back to any output value. Multi-output derivations
    // share the same name stem.
    let value = outs.get("out").or_else(|| outs.values().next())?;
    let last = Path::new(value).file_name()?.to_str()?;
    let (hash, rest) = last.split_once('-')?;
    if hash.len() != 32 {
        return None;
    }
    Some(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{
        any_value::Value as AnyValueOneof, AnyValue, KeyValue,
    };
    use opentelemetry_proto::tonic::trace::v1::Status;

    fn make_span_with_name(name: &str, code: i32) -> Span {
        Span {
            trace_id: vec![1; 16],
            span_id: vec![2; 8],
            parent_span_id: vec![],
            name: format!("nix.build.{name}"),
            attributes: vec![KeyValue {
                key: "nix.derivation.name".into(),
                value: Some(AnyValue {
                    value: Some(AnyValueOneof::StringValue(name.into())),
                }),
                ..Default::default()
            }],
            status: Some(Status {
                message: "build reported a failure log line".into(),
                code,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn derives_name_from_outputs() {
        let mut m = HashMap::new();
        m.insert(
            "out".to_string(),
            "/nix/store/grsw46wjmja12bzbjg3mj4f562r1g4s6-demo-workspace-doc-0.1.0"
                .to_string(),
        );
        assert_eq!(
            derivation_name_from_outputs(&Some(m)),
            Some("demo-workspace-doc-0.1.0".to_string()),
        );
    }

    #[test]
    fn no_outputs_returns_none() {
        assert_eq!(derivation_name_from_outputs(&None), None);
    }

    #[test]
    fn non_store_path_returns_none() {
        let mut m = HashMap::new();
        m.insert("out".to_string(), "not-a-store-path".to_string());
        assert_eq!(derivation_name_from_outputs(&Some(m)), None);
    }

    #[test]
    fn falls_back_to_any_output_when_no_out() {
        let mut m = HashMap::new();
        m.insert(
            "dev".to_string(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-libfoo-1.0-dev".to_string(),
        );
        assert_eq!(
            derivation_name_from_outputs(&Some(m)),
            Some("libfoo-1.0-dev".to_string()),
        );
    }

    #[tokio::test]
    async fn reconcile_flips_error_to_ok_when_json_says_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("aa").join("bb");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let span_path = dir.join("span.json");

        let span = make_span_with_name("demo-workspace-doc-0.1.0", 2);
        let bytes = serde_json::to_vec_pretty(&span).unwrap();
        tokio::fs::write(&span_path, bytes).await.unwrap();

        let mut verdicts = HashMap::new();
        verdicts.insert("demo-workspace-doc-0.1.0".into(), true);

        let changed = reconcile_one(&span_path, &verdicts).await.unwrap();
        assert!(changed, "span should be rewritten");

        let after_bytes = tokio::fs::read(&span_path).await.unwrap();
        let after: Span = serde_json::from_slice(&after_bytes).unwrap();
        assert_eq!(after.status.as_ref().unwrap().code, 1, "should now be Ok");
        assert!(
            after.status.unwrap().message.is_empty(),
            "Ok status should clear the heuristic message"
        );
    }

    #[tokio::test]
    async fn reconcile_flips_ok_to_error_when_json_says_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("aa").join("bb");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let span_path = dir.join("span.json");

        let span = make_span_with_name("demo-workspace-fmt-0.1.0", 1);
        let bytes = serde_json::to_vec_pretty(&span).unwrap();
        tokio::fs::write(&span_path, bytes).await.unwrap();

        let mut verdicts = HashMap::new();
        verdicts.insert("demo-workspace-fmt-0.1.0".into(), false);

        let changed = reconcile_one(&span_path, &verdicts).await.unwrap();
        assert!(changed, "span should be rewritten");

        let after_bytes = tokio::fs::read(&span_path).await.unwrap();
        let after: Span = serde_json::from_slice(&after_bytes).unwrap();
        assert_eq!(after.status.as_ref().unwrap().code, 2, "should now be Error");
    }

    #[tokio::test]
    async fn reconcile_skips_span_without_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("aa").join("bb");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let span_path = dir.join("span.json");

        let span = make_span_with_name("some-other-derivation", 2);
        let bytes = serde_json::to_vec_pretty(&span).unwrap();
        tokio::fs::write(&span_path, bytes).await.unwrap();

        let mut verdicts = HashMap::new();
        verdicts.insert("demo-workspace-doc-0.1.0".into(), true);

        let changed = reconcile_one(&span_path, &verdicts).await.unwrap();
        assert!(!changed, "no-match span should be left alone");
    }

    #[tokio::test]
    async fn reconcile_no_op_when_status_already_matches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("aa").join("bb");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let span_path = dir.join("span.json");

        // Span already has code=1 (Ok); JSON says success — nothing to do.
        let span = make_span_with_name("demo-workspace-doc-0.1.0", 1);
        let bytes = serde_json::to_vec_pretty(&span).unwrap();
        tokio::fs::write(&span_path, bytes).await.unwrap();

        let mut verdicts = HashMap::new();
        verdicts.insert("demo-workspace-doc-0.1.0".into(), true);

        let changed = reconcile_one(&span_path, &verdicts).await.unwrap();
        assert!(!changed, "no-op when status already correct");
    }

    #[tokio::test]
    async fn walk_handles_missing_spans_dir_gracefully() {
        let verdicts = HashMap::new();
        let (s, u) = walk_and_reconcile(Path::new("/nonexistent-spans-dir-xyz"), &verdicts)
            .await
            .unwrap();
        assert_eq!((s, u), (0, 0));
    }
}
