//! Shared directory layout for OTLP/JSON disk output.
//!
//! Go reference: `otelcli/server_json.go::renderJson`. Both the `json+file`
//! client and the `server json` subcommand use this same layout:
//!
//! ```text
//! <root>/<traceHex>/<spanHex>/span.json
//! <root>/<traceHex>/<spanHex>/event-N.json
//! ```
//!
//! Trace and span IDs are hex-encoded (32 / 16 chars). Events are zero-indexed
//! in span.events ordering.
//!
//! ### Durable writes (PRD §9.1)
//!
//! When the `durable` flag is set, each write follows the
//! open → `write_all` → `flush` → `fdatasync` sequence, and the parent
//! directory is `fsync`'d after the last entry. This is what makes the
//! file leg of the tee survive process / host crashes mid-pipeline.

use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, Span};
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Write every span in `rs` and its events into `<root>/<traceHex>/<spanHex>/`.
pub async fn write_resource_spans_to_dir(
    root: &Path,
    rs: &ResourceSpans,
    durable: bool,
) -> std::io::Result<()> {
    for ss in &rs.scope_spans {
        for span in &ss.spans {
            write_span_to_dir(root, span, durable).await?;
        }
    }
    Ok(())
}

/// Write a single span (and its events) to `<root>/<traceHex>/<spanHex>/`.
/// Mirrors Go's `renderJson`. When `durable` is true, every file is
/// flushed + fdatasync'd and the containing directory is fsync'd.
pub async fn write_span_to_dir(
    root: &Path,
    span: &Span,
    durable: bool,
) -> std::io::Result<()> {
    let trace_hex = hex::encode(&span.trace_id);
    let span_hex = hex::encode(&span.span_id);

    let dir = root.join(trace_hex).join(span_hex);
    tokio::fs::create_dir_all(&dir).await?;

    let span_json = serde_json::to_vec_pretty(span)
        .map_err(|e| std::io::Error::other(format!("span: {e}")))?;
    write_file_durable(&dir.join("span.json"), &span_json, durable).await?;

    for (i, event) in span.events.iter().enumerate() {
        let bytes = serde_json::to_vec_pretty(event)
            .map_err(|e| std::io::Error::other(format!("event-{i}: {e}")))?;
        write_file_durable(&dir.join(format!("event-{i}.json")), &bytes, durable).await?;
    }

    // Make the dirent durable as well — fdatasync on the regular file isn't
    // enough; the directory entry itself lives in the parent inode's data.
    if durable {
        fsync_dir(&dir).await?;
    }

    Ok(())
}

/// Write `bytes` to `path` atomically-ish: open + write_all + flush + (optional)
/// fdatasync. We don't use a temp-file rename because the existing layout
/// expects fixed filenames inside a per-span directory; the per-span dir is
/// effectively the unit of atomicity, and crash recovery sees partial dirs
/// as "skip this span".
pub(crate) async fn write_file_durable(
    path: &Path,
    bytes: &[u8],
    durable: bool,
) -> std::io::Result<()> {
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await?;
    write_and_sync(f, bytes, durable).await
}

/// Append `bytes` to `path`, creating it if necessary, then (optionally)
/// fdatasync the file and fsync the parent directory. This is the durable
/// primitive the checkpoint log builds on: each record is one `write_all`
/// of a single line, made durable before the caller is told it succeeded.
///
/// The parent-dir fsync is what makes a *newly created* run file's dirent
/// survive a crash; without it ext4 `data=ordered` can lose the filename even
/// though the file's contents were fdatasync'd.
pub(crate) async fn append_line_durable(
    path: &Path,
    bytes: &[u8],
    durable: bool,
) -> std::io::Result<()> {
    let f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .await?;
    write_and_sync(f, bytes, durable).await?;
    if durable {
        if let Some(parent) = path.parent() {
            fsync_dir(parent).await?;
        }
    }
    Ok(())
}

/// Shared tail of the durable-write path: write_all + flush + (optional)
/// fdatasync. Takes ownership of the open file so it can hand the fd to
/// `spawn_blocking` for the sync syscall.
async fn write_and_sync(
    mut f: tokio::fs::File,
    bytes: &[u8],
    durable: bool,
) -> std::io::Result<()> {
    f.write_all(bytes).await?;
    f.flush().await?;

    if durable {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let std_file = f.into_std().await;
            let raw = std_file.as_raw_fd();
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                nix::unistd::fdatasync(raw).map_err(std::io::Error::from)?;
                // Closing the std::fs::File at end-of-scope drops the fd
                // cleanly. We must keep `std_file` alive through fdatasync.
                drop(std_file);
                Ok(())
            })
            .await
            .map_err(std::io::Error::other)??;
        }
        #[cfg(not(unix))]
        {
            // No fdatasync on non-Unix targets; flush already happened above.
            // Durable mode is best-effort outside Unix.
            let _ = f;
        }
    }
    Ok(())
}

/// fsync the directory inode so the dirent (filename → inode) is durable.
/// Linux requires this in addition to fdatasync on the file itself.
#[cfg(unix)]
pub(crate) async fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let path = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let f = std::fs::File::open(&path)?;
        nix::unistd::fsync(f.as_raw_fd()).map_err(std::io::Error::from)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)??;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) async fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{
        any_value::Value as AnyValueOneof, AnyValue, KeyValue,
    };
    use opentelemetry_proto::tonic::trace::v1::{span::Event, ScopeSpans, Span};

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
            name: "layout-test".into(),
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
    async fn writes_expected_file_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rs = make_span_with_events(2);
        write_resource_spans_to_dir(tmp.path(), &rs, false).await.unwrap();

        let span_dir = tmp
            .path()
            .join("0af7651916cd43dd8448eb211c80319c")
            .join("b7ad6b7169203331");
        let span_path = span_dir.join("span.json");
        assert!(span_path.exists(), "{span_path:?} should exist");

        let txt = tokio::fs::read_to_string(&span_path).await.unwrap();
        assert!(txt.contains("layout-test"));
        assert!(txt.contains("0af7651916cd43dd8448eb211c80319c"));

        for i in 0..2 {
            let event_path = span_dir.join(format!("event-{i}.json"));
            assert!(event_path.exists(), "{event_path:?} should exist");
            let etxt = tokio::fs::read_to_string(&event_path).await.unwrap();
            assert!(etxt.contains(&format!("event-name-{i}")));
        }
    }

    #[tokio::test]
    async fn no_events_writes_only_span() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rs = make_span_with_events(0);
        write_resource_spans_to_dir(tmp.path(), &rs, false).await.unwrap();

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

    /// Exercises the fdatasync/fsync-dir path. We can't really observe the
    /// syscall here without strace, but the contract is "doesn't error and
    /// produces the same file tree".
    #[cfg(unix)]
    #[tokio::test]
    async fn durable_mode_writes_same_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rs = make_span_with_events(2);
        write_resource_spans_to_dir(tmp.path(), &rs, true).await.unwrap();

        let span_dir = tmp
            .path()
            .join("0af7651916cd43dd8448eb211c80319c")
            .join("b7ad6b7169203331");
        assert!(span_dir.join("span.json").exists());
        assert!(span_dir.join("event-0.json").exists());
        assert!(span_dir.join("event-1.json").exists());
    }
}
