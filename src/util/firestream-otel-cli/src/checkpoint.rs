//! Crash-durable span checkpoint log (PRD §9.3).
//!
//! Each `span background` open appends an `open` record to
//! `${OTEL_CHECKPOINT_DIR}/<run_id>.jsonl`, fdatasync'd + dir-fsync'd before
//! the daemon acks the caller. Clean close appends a `close` record. On
//! startup, opens without a matching close are replayed as orphan spans with
//! `run.recovered=true`.
//!
//! ### File layout
//!
//! ```text
//! ${OTEL_CHECKPOINT_DIR}/<run_id>.jsonl        — active checkpoint log
//! ${OTEL_CHECKPOINT_DIR}/<run_id>.jsonl.done   — processed (post-replay)
//! ```
//!
//! Each line is one JSON-encoded [`CheckpointRecord`]. Records are paired by
//! `span_id`: an `open` with no later `close` for the same span is an orphan.
//!
//! ### Double-emit guard
//!
//! Both the in-process startup hook and the host-side replay daemon scan the
//! same directory. The replay routine acquires a non-blocking `flock(2)` on
//! each `.jsonl` file before processing and renames it to `.jsonl.done` on
//! success. Whoever loses the lock skips the file; whoever renames it removes
//! it from the candidate set entirely.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use serde::{Deserialize, Serialize};

use crate::json_layout::append_line_durable;

/// Directory ceiling: total checkpoint dir size before the oldest run files
/// are deleted (PRD §13 rotation).
pub const DIR_CEILING_BYTES: u64 = 100 * 1024 * 1024;

/// Per-run file cap: once a single `<run_id>.jsonl` exceeds this, a
/// `truncated` marker is appended and further appends are refused.
pub const FILE_CAP_BYTES: u64 = 50 * 1024 * 1024;

/// A single line in a checkpoint log. The `kind` tag is the discriminant so a
/// human can `grep` the file. `Open`/`Close` carry enough to rebuild a `Span`
/// for orphan replay without re-reading any other state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CheckpointRecord {
    /// A background span was opened. Written durably before the daemon acks.
    Open {
        /// 32-char lowercase hex.
        trace_id: String,
        /// 16-char lowercase hex.
        span_id: String,
        /// 16-char lowercase hex; absent when the span is a trace root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_span_id: Option<String>,
        name: String,
        service: String,
        /// Span start time, unix nanoseconds.
        start_unix_nano: u64,
        #[serde(default)]
        attrs: BTreeMap<String, String>,
    },
    /// The span closed cleanly. Pairs with an `Open` by `span_id`.
    Close {
        trace_id: String,
        span_id: String,
        /// Span end time, unix nanoseconds.
        end_unix_nano: u64,
        /// otel status code: "unset" | "ok" | "error".
        status: String,
        /// End-time attributes merged into the final span. `#[serde(default)]`
        /// so older checkpoint files (which did not include this field) still
        /// deserialize cleanly during orphan replay — PRD §9.3 requires the
        /// on-disk checkpoint format to remain stable across binary versions.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        attrs: BTreeMap<String, String>,
    },
    /// Marker emitted when a run file hit [`FILE_CAP_BYTES`]. Stops the
    /// scanner from trusting any later bytes in the file.
    Truncated,
}

/// Compose `<dir>/<run_id>.jsonl`.
pub fn run_file(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("{run_id}.jsonl"))
}

/// Append an `open` record for `run_id`, durably (fdatasync + parent-dir
/// fsync) when `durable` is set. The directory is created if absent. Honours
/// the per-run file cap: if the run file is already over [`FILE_CAP_BYTES`],
/// a [`CheckpointRecord::Truncated`] marker is appended instead and the open
/// is dropped (the in-flight span will simply not be checkpointed).
pub async fn append_open(
    dir: &Path,
    run_id: &str,
    rec: &CheckpointRecord,
    durable: bool,
) -> io::Result<()> {
    append_record(dir, run_id, rec, durable).await
}

/// Append a `close` record for `run_id`, durably when `durable` is set.
pub async fn append_close(
    dir: &Path,
    run_id: &str,
    rec: &CheckpointRecord,
    durable: bool,
) -> io::Result<()> {
    append_record(dir, run_id, rec, durable).await
}

async fn append_record(
    dir: &Path,
    run_id: &str,
    rec: &CheckpointRecord,
    durable: bool,
) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let path = run_file(dir, run_id);

    // Per-run cap: once over, write a one-time truncation marker and refuse
    // further appends. The marker itself is small enough to always fit.
    if let Ok(meta) = tokio::fs::metadata(&path).await {
        if meta.len() >= FILE_CAP_BYTES {
            enforce_file_cap(&path, durable).await?;
            return Ok(());
        }
    }

    let mut line = serde_json::to_vec(rec).map_err(|e| io::Error::other(format!("checkpoint: {e}")))?;
    line.push(b'\n');
    append_line_durable(&path, &line, durable).await
}

/// Append a [`CheckpointRecord::Truncated`] marker exactly once. Idempotent:
/// if the file already ends with a truncation marker we skip re-appending.
async fn enforce_file_cap(path: &Path, durable: bool) -> io::Result<()> {
    // Cheap idempotency: read the last line and bail if it's already a marker.
    if let Ok(text) = tokio::fs::read_to_string(path).await {
        if let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) {
            if let Ok(CheckpointRecord::Truncated) = serde_json::from_str::<CheckpointRecord>(last) {
                return Ok(());
            }
        }
    }
    let mut line =
        serde_json::to_vec(&CheckpointRecord::Truncated).map_err(|e| io::Error::other(format!("checkpoint: {e}")))?;
    line.push(b'\n');
    append_line_durable(path, &line, durable).await
}

/// Public wrapper for the per-run file cap (PRD §13). Appends a truncation
/// marker when `path` exceeds [`FILE_CAP_BYTES`]; otherwise a no-op.
pub async fn enforce_file_cap_if_over(path: &Path, durable: bool) -> io::Result<()> {
    if let Ok(meta) = tokio::fs::metadata(path).await {
        if meta.len() >= FILE_CAP_BYTES {
            return enforce_file_cap(path, durable).await;
        }
    }
    Ok(())
}

/// Scan every `*.jsonl` checkpoint file (NOT `*.jsonl.done`) in `dir` and
/// return the `open` records that have no matching `close` for the same
/// `span_id`. A [`CheckpointRecord::Truncated`] marker stops parsing the rest
/// of that file (later bytes may be a torn write).
///
/// Malformed lines are skipped rather than aborting the scan — a partially
/// written final line is expected after a crash mid-append.
pub fn scan_orphans(dir: &Path) -> Vec<CheckpointRecord> {
    let mut orphans = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return orphans,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_active_jsonl(&path) {
            continue;
        }
        orphans.extend(scan_orphans_in_file(&path));
    }
    orphans
}

/// Return true for `*.jsonl` files (the active set), false for `*.jsonl.done`
/// and everything else.
fn is_active_jsonl(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|e| e == "jsonl")
            .unwrap_or(false)
}

/// Parse a single checkpoint file and return its unmatched opens.
pub fn scan_orphans_in_file(path: &Path) -> Vec<CheckpointRecord> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut opens: Vec<CheckpointRecord> = Vec::new();
    let mut closed: HashSet<String> = HashSet::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: CheckpointRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            // Torn final write or schema drift — skip the line, keep scanning.
            Err(_) => continue,
        };
        match rec {
            CheckpointRecord::Open { .. } => opens.push(rec),
            CheckpointRecord::Close { ref span_id, .. } => {
                closed.insert(span_id.clone());
            }
            CheckpointRecord::Truncated => break,
        }
    }

    opens
        .into_iter()
        .filter(|o| match o {
            CheckpointRecord::Open { span_id, .. } => !closed.contains(span_id),
            _ => false,
        })
        .collect()
}

/// Delete oldest-run files (by mtime) until the total size of `dir` is at or
/// below `ceiling`. Only `*.jsonl` and `*.jsonl.done` files are counted and
/// eligible for deletion. Returns the number of files deleted.
pub fn enforce_dir_ceiling(dir: &Path, ceiling: u64) -> io::Result<usize> {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_checkpoint_file(&path) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
            files.push((path, meta.len(), mtime));
        }
    }

    let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= ceiling {
        return Ok(0);
    }

    // Oldest first, so deletion preserves the most recent runs.
    files.sort_by_key(|(_, _, mtime)| *mtime);

    let mut deleted = 0;
    for (path, len, _) in files {
        if total <= ceiling {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// True for any `.jsonl` or `.jsonl.done` checkpoint file.
fn is_checkpoint_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name.ends_with(".jsonl") || name.ends_with(".jsonl.done"),
        None => false,
    }
}

/// Current unix time in nanoseconds (saturating).
pub fn now_unix_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

/// Map an otel status string back to its enum.
fn status_from_str(s: &str) -> StatusCode {
    match s.to_ascii_lowercase().as_str() {
        "ok" => StatusCode::Ok,
        "error" => StatusCode::Error,
        _ => StatusCode::Unset,
    }
}

#[cfg(unix)]
mod replay_unix {
    //! Replay routine shared by the `server json` startup hook and the
    //! standalone `replay` subcommand. Both call [`replay_dir`].

    use super::*;
    use crate::client::build_client;
    use crate::config::Config;
    use opentelemetry_proto::tonic::common::v1::{
        any_value::Value as AnyValueOneof, AnyValue, InstrumentationScope, KeyValue,
    };
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{
        span::SpanKind, ResourceSpans, ScopeSpans, Span, Status,
    };
    use std::os::unix::io::AsRawFd;
    use std::time::Duration;

    /// Summary of one replay pass, returned for logging/testing.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct ReplayReport {
        /// Files we locked + processed + renamed to `.jsonl.done`.
        pub files_processed: usize,
        /// Files skipped because another processor held the lock.
        pub files_locked_out: usize,
        /// Orphan spans emitted across all processed files.
        pub orphans_emitted: usize,
        /// Files deleted by the dir-ceiling sweep at the end.
        pub files_rotated: usize,
    }

    /// Process every active `*.jsonl` in `dir`: acquire a non-blocking
    /// exclusive `flock`, scan orphans, emit them via `cfg`'s client with
    /// `run.recovered=true`, then rename the file to `*.jsonl.done`. Files
    /// older than `min_age` (by mtime) are eligible; pass `Duration::ZERO`
    /// to process everything (the startup hook does this).
    ///
    /// `cfg` selects the OTLP transport (and tee mode) the orphans are sent
    /// through — the same `build_client` path as live spans.
    pub async fn replay_dir(
        dir: &Path,
        cfg: &Config,
        min_age: Duration,
    ) -> anyhow::Result<ReplayReport> {
        let mut report = ReplayReport::default();
        if tokio::fs::metadata(dir).await.is_err() {
            return Ok(report);
        }

        let candidates = collect_candidates(dir, min_age);

        for path in candidates {
            // Acquire the per-file lock without blocking. The held File must
            // outlive processing — drop unlocks it (and a successful rename
            // removes the dirent regardless).
            let locked = match lock_nonblocking(&path) {
                Some(f) => f,
                None => {
                    report.files_locked_out += 1;
                    continue;
                }
            };

            let orphans = scan_orphans_in_file(&path);
            if !orphans.is_empty() {
                let rs = orphans_to_resource_spans(&orphans);
                emit(cfg, rs).await?;
                report.orphans_emitted += orphans.len();
            }

            // Rename to .done so neither processor revisits it. Renaming the
            // path the lock is on is fine: the fd (and thus the lock) refers
            // to the inode, not the name.
            let done = with_done_suffix(&path);
            if let Err(e) = tokio::fs::rename(&path, &done).await {
                eprintln!("otel-cli replay: rename {path:?} -> {done:?} failed: {e}");
            } else {
                report.files_processed += 1;
            }
            drop(locked);
        }

        report.files_rotated = enforce_dir_ceiling(dir, DIR_CEILING_BYTES)?;
        Ok(report)
    }

    /// List `*.jsonl` files at least `min_age` old (by mtime).
    fn collect_candidates(dir: &Path, min_age: Duration) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return out,
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_active_jsonl(&path) {
                continue;
            }
            if !min_age.is_zero() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if now.duration_since(modified).unwrap_or(Duration::ZERO) < min_age {
                            continue;
                        }
                    }
                }
            }
            out.push(path);
        }
        out
    }

    /// Acquire `LOCK_EX | LOCK_NB` on `path`. Returns the open file (whose fd
    /// holds the advisory lock — drop or close to release) on success, `None`
    /// if another holder owns it.
    ///
    /// We use `nix::fcntl::flock` (the free function) rather than the owned
    /// `nix::fcntl::Flock`: the latter takes ownership and *panics* if the
    /// unlock-at-drop fails, which is the wrong shape here because we rename
    /// the file out from under the lock. flock is fd-scoped, so the lock
    /// follows the inode and is released when the fd closes.
    #[allow(deprecated)]
    fn lock_nonblocking(path: &Path) -> Option<std::fs::File> {
        use nix::fcntl::{flock, FlockArg};
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()?;
        match flock(f.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
            Ok(()) => Some(f),
            // EWOULDBLOCK / EAGAIN => another processor owns it.
            Err(_) => None,
        }
    }

    fn with_done_suffix(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(".done");
        PathBuf::from(s)
    }

    /// Build a `ResourceSpans` from orphan opens, marking each `run.recovered`.
    /// End time is "now" and status is ERROR (the real outcome is unknown —
    /// the process died before writing a close).
    fn orphans_to_resource_spans(orphans: &[CheckpointRecord]) -> ResourceSpans {
        let now = now_unix_nano();
        let mut service = "otel-cli".to_string();
        let mut spans = Vec::new();

        for rec in orphans {
            if let CheckpointRecord::Open {
                trace_id,
                span_id,
                parent_span_id,
                name,
                service: svc,
                start_unix_nano,
                attrs,
            } = rec
            {
                service = svc.clone();
                let mut attributes: Vec<KeyValue> = attrs
                    .iter()
                    .map(|(k, v)| KeyValue {
                        key: k.clone(),
                        value: Some(crate::span::string_to_any_value(v)),
                        ..Default::default()
                    })
                    .collect();
                attributes.push(KeyValue {
                    key: "run.recovered".to_string(),
                    value: Some(AnyValue {
                        value: Some(AnyValueOneof::BoolValue(true)),
                    }),
                    ..Default::default()
                });

                spans.push(Span {
                    trace_id: hex::decode(trace_id).unwrap_or_default(),
                    span_id: hex::decode(span_id).unwrap_or_default(),
                    trace_state: String::new(),
                    parent_span_id: parent_span_id
                        .as_deref()
                        .and_then(|p| hex::decode(p).ok())
                        .unwrap_or_default(),
                    flags: 0,
                    name: name.clone(),
                    kind: SpanKind::Internal as i32,
                    start_time_unix_nano: *start_unix_nano,
                    end_time_unix_nano: now,
                    attributes,
                    dropped_attributes_count: 0,
                    events: Vec::new(),
                    dropped_events_count: 0,
                    links: Vec::new(),
                    dropped_links_count: 0,
                    status: Some(Status {
                        message: "recovered orphan span (no clean close)".to_string(),
                        code: status_from_str("error") as i32,
                    }),
                });
            }
        }

        ResourceSpans {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_string(),
                    value: Some(AnyValue {
                        value: Some(AnyValueOneof::StringValue(service)),
                    }),
                    ..Default::default()
                }],
                dropped_attributes_count: 0,
                entity_refs: Vec::new(),
            }),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: "otel-cli".to_string(),
                    version: String::new(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }
    }

    async fn emit(cfg: &Config, rs: ResourceSpans) -> anyhow::Result<()> {
        use anyhow::Context;
        let mut client = build_client(cfg);
        client.start().await.context("replay client start")?;
        client
            .upload_traces(vec![rs])
            .await
            .context("replay upload")?;
        client.stop().await.context("replay client stop")?;
        Ok(())
    }
}

#[cfg(unix)]
pub use replay_unix::{replay_dir, ReplayReport};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_rec(span: &str, trace: &str, name: &str) -> CheckpointRecord {
        CheckpointRecord::Open {
            trace_id: trace.to_string(),
            span_id: span.to_string(),
            parent_span_id: None,
            name: name.to_string(),
            service: "svc".to_string(),
            start_unix_nano: 1_000,
            attrs: BTreeMap::new(),
        }
    }

    fn close_rec(span: &str, trace: &str) -> CheckpointRecord {
        CheckpointRecord::Close {
            trace_id: trace.to_string(),
            span_id: span.to_string(),
            end_unix_nano: 2_000,
            status: "ok".to_string(),
            attrs: BTreeMap::new(),
        }
    }

    fn write_jsonl(path: &Path, recs: &[CheckpointRecord]) {
        let mut buf = String::new();
        for r in recs {
            buf.push_str(&serde_json::to_string(r).unwrap());
            buf.push('\n');
        }
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn scan_orphans_returns_unmatched_open() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        // span "aaaa..." opened but never closed → orphan.
        // span "bbbb..." opened and closed → not an orphan.
        write_jsonl(
            &run_file(tmp.path(), "run-1"),
            &[
                open_rec("aaaaaaaaaaaaaaaa", trace, "orphaned"),
                open_rec("bbbbbbbbbbbbbbbb", trace, "completed"),
                close_rec("bbbbbbbbbbbbbbbb", trace),
            ],
        );

        let orphans = scan_orphans(tmp.path());
        assert_eq!(orphans.len(), 1, "exactly one orphan expected");
        match &orphans[0] {
            CheckpointRecord::Open { span_id, name, .. } => {
                assert_eq!(span_id, "aaaaaaaaaaaaaaaa");
                assert_eq!(name, "orphaned");
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn scan_orphans_open_with_close_not_returned() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        write_jsonl(
            &run_file(tmp.path(), "run-1"),
            &[
                open_rec("cccccccccccccccc", trace, "done"),
                close_rec("cccccccccccccccc", trace),
            ],
        );
        let orphans = scan_orphans(tmp.path());
        assert!(orphans.is_empty(), "closed span must not be an orphan");
    }

    #[test]
    fn scan_orphans_ignores_done_files() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        // A .jsonl.done file with an unmatched open must be ignored.
        let done = tmp.path().join("run-old.jsonl.done");
        write_jsonl(&done, &[open_rec("dddddddddddddddd", trace, "old")]);
        let orphans = scan_orphans(tmp.path());
        assert!(orphans.is_empty(), ".done files are not scanned");
    }

    #[test]
    fn scan_orphans_stops_at_truncated_marker() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        // open after a truncation marker must not be trusted.
        write_jsonl(
            &run_file(tmp.path(), "run-trunc"),
            &[
                open_rec("eeeeeeeeeeeeeeee", trace, "before"),
                close_rec("eeeeeeeeeeeeeeee", trace),
                CheckpointRecord::Truncated,
                open_rec("ffffffffffffffff", trace, "after-trunc"),
            ],
        );
        let orphans = scan_orphans(tmp.path());
        assert!(
            orphans.is_empty(),
            "records after a truncated marker are not scanned"
        );
    }

    #[test]
    fn scan_orphans_skips_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        let path = run_file(tmp.path(), "run-torn");
        let mut buf = String::new();
        buf.push_str(&serde_json::to_string(&open_rec("1111111111111111", trace, "ok")).unwrap());
        buf.push('\n');
        // torn final line (crash mid-append)
        buf.push_str("{\"kind\":\"open\",\"trace_id\":\"0af");
        std::fs::write(&path, buf).unwrap();

        let orphans = scan_orphans(tmp.path());
        assert_eq!(orphans.len(), 1, "valid open survives a torn trailing line");
    }

    #[test]
    fn enforce_dir_ceiling_deletes_oldest() {
        let tmp = TempDir::new().unwrap();
        // Three ~equal files; ceiling forces deleting the oldest one(s).
        let payload = vec![b'x'; 4096];
        for name in ["a", "b", "c"] {
            let p = run_file(tmp.path(), name);
            std::fs::write(&p, &payload).unwrap();
        }
        // Stagger mtimes: a oldest, c newest.
        set_mtime(&run_file(tmp.path(), "a"), 1_000);
        set_mtime(&run_file(tmp.path(), "b"), 2_000);
        set_mtime(&run_file(tmp.path(), "c"), 3_000);

        // Ceiling that fits ~2 files → oldest ("a") gets deleted.
        let ceiling = 4096 * 2 + 100;
        let deleted = enforce_dir_ceiling(tmp.path(), ceiling).unwrap();
        assert_eq!(deleted, 1, "exactly the oldest file should be deleted");
        assert!(!run_file(tmp.path(), "a").exists(), "oldest deleted");
        assert!(run_file(tmp.path(), "b").exists(), "newer kept");
        assert!(run_file(tmp.path(), "c").exists(), "newest kept");
    }

    #[test]
    fn enforce_dir_ceiling_noop_under_ceiling() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(run_file(tmp.path(), "small"), b"hi").unwrap();
        let deleted = enforce_dir_ceiling(tmp.path(), DIR_CEILING_BYTES).unwrap();
        assert_eq!(deleted, 0);
        assert!(run_file(tmp.path(), "small").exists());
    }

    #[tokio::test]
    async fn append_open_then_close_pairs_cleanly() {
        let tmp = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        let span = "9999999999999999";
        append_open(tmp.path(), "run-x", &open_rec(span, trace, "n"), false)
            .await
            .unwrap();
        // After open only → orphan.
        assert_eq!(scan_orphans(tmp.path()).len(), 1);

        append_close(tmp.path(), "run-x", &close_rec(span, trace), false)
            .await
            .unwrap();
        // After close → no orphan.
        assert!(scan_orphans(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replay_dir_emits_orphan_and_renames_file() {
        use crate::config::Config;
        use std::time::Duration;

        let chkdir = TempDir::new().unwrap();
        let outdir = TempDir::new().unwrap();
        let trace = "0af7651916cd43dd8448eb211c80319c";
        let span = "1234567812345678";

        // One orphan open (no close) in an active .jsonl.
        write_jsonl(
            &run_file(chkdir.path(), "run-orphan"),
            &[open_rec(span, trace, "killed-build")],
        );

        // Replay into a json+file sink.
        let mut cfg = Config::defaults();
        cfg.protocol = "json+file".to_string();
        cfg.json_dir = outdir.path().to_string_lossy().to_string();

        let report = replay_dir(chkdir.path(), &cfg, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report.orphans_emitted, 1, "one orphan emitted");
        assert_eq!(report.files_processed, 1, "one file processed");

        // The recovered span landed with run.recovered=true.
        let span_path = outdir
            .path()
            .join(trace)
            .join(span)
            .join("span.json");
        assert!(span_path.exists(), "recovered span.json should exist");
        let txt = std::fs::read_to_string(&span_path).unwrap();
        assert!(txt.contains("run.recovered"), "marker attribute present");
        assert!(txt.contains("killed-build"), "original span name preserved");

        // The source file was renamed to .done so it isn't re-processed.
        assert!(
            !run_file(chkdir.path(), "run-orphan").exists(),
            "active .jsonl removed"
        );
        assert!(
            chkdir.path().join("run-orphan.jsonl.done").exists(),
            ".done file created"
        );

        // A second replay pass is a clean no-op (double-emit guard via rename).
        let report2 = replay_dir(chkdir.path(), &cfg, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report2.orphans_emitted, 0, "no double-emit on second pass");
        assert_eq!(report2.files_processed, 0);
    }

    fn set_mtime(path: &Path, secs: i64) {
        // Use filetime-free approach via utimensat through std is unstable;
        // shell out to `touch -d` would add a dependency. Instead, rely on
        // write-order mtimes being monotonic by sleeping is flaky — so set
        // mtime directly via the `nix` crate which is already a dep.
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let f = std::fs::File::open(path).unwrap();
            let times = [
                nix::sys::time::TimeSpec::new(secs, 0),
                nix::sys::time::TimeSpec::new(secs, 0),
            ];
            // futimens(fd, [atime, mtime])
            nix::sys::stat::futimens(f.as_raw_fd(), &times[0], &times[1]).unwrap();
        }
    }
}
