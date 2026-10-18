//! Background span server: accepts client RPCs over a unix socket while
//! holding a single span open.
//!
//! Go reference: `otelcli/span_background_server.go`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::trace::v1::{span::Event, ResourceSpans, Status};
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};

use super::protocol::{read_frame, write_frame, Request, Response};
use crate::checkpoint::{self, CheckpointRecord};
use crate::client::build_client;
use crate::config::{parse_cli_time, Config};
use crate::span::{self as span_builder, attrs_to_keyvalues, status_code_from_str};
use crate::traceparent::Traceparent;

/// Default socket filename inside the user-supplied sockdir.
pub const SOCK_FILENAME: &str = "otel-cli.sock";

/// Compose `<sockdir>/otel-cli.sock`.
pub fn socket_path(sockdir: &Path) -> PathBuf {
    sockdir.join(SOCK_FILENAME)
}

/// Shared state held across all client connections + the watcher tasks.
struct BgState {
    /// Initial `ResourceSpans` payload built from the foreground config.
    /// Events get appended to `scope_spans[0].spans[0].events`; on End we
    /// overwrite the inner span's end_time, attributes, and status.
    rs: ResourceSpans,
    /// Set true once End handler ran or a watcher tripped — used to fence
    /// further mutations from late-arriving connections.
    ended: bool,
}

impl BgState {
    fn trace_id_hex(&self) -> String {
        hex::encode(&self.rs.scope_spans[0].spans[0].trace_id)
    }
    fn span_id_hex(&self) -> String {
        hex::encode(&self.rs.scope_spans[0].spans[0].span_id)
    }
    fn traceparent(&self, recording: bool) -> String {
        let span = &self.rs.scope_spans[0].spans[0];
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        let tl = trace_id.len().min(span.trace_id.len());
        trace_id[..tl].copy_from_slice(&span.trace_id[..tl]);
        let sl = span_id.len().min(span.span_id.len());
        span_id[..sl].copy_from_slice(&span.span_id[..sl]);
        Traceparent {
            version: 0,
            trace_id,
            span_id,
            flags: if recording { 0x01 } else { 0x00 },
            initialized: true,
        }
        .encode()
    }
}

/// Run the background server until End is called, ctrl-c is received, or the
/// parent process pid changes. Cleans up the socket file before returning.
pub async fn run_server(cfg: Config, sockdir: PathBuf) -> Result<()> {
    // 1) prepare socket
    tokio::fs::create_dir_all(&sockdir)
        .await
        .with_context(|| format!("creating sockdir {sockdir:?}"))?;
    let sock_path = socket_path(&sockdir);
    if tokio::fs::metadata(&sock_path).await.is_ok() {
        let _ = tokio::fs::remove_file(&sock_path).await;
    }
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding unix socket {sock_path:?}"))?;
    eprintln!(
        "otel-cli span background: listening on {}",
        sock_path.display()
    );

    // 2) seed state with the initial span
    let rs = span_builder::build_resource_spans(&cfg);
    let state = Arc::new(Mutex::new(BgState { rs, ended: false }));
    let recording = cfg.is_recording();

    // 2a) Crash-durable checkpoint (PRD §9.3). Append the `open` record and
    // **await it before** the accept loop starts — i.e. before any client RPC
    // (and thus any ack) can observe the running span. If the daemon is
    // SIGKILL'd between here and a clean close, startup replay re-emits this
    // span with run.recovered=true. Opt-in via OTEL_CHECKPOINT_DIR; unset is a
    // no-op so non-CI use pays nothing.
    let checkpoint_dir = cfg.resolved_checkpoint_dir();
    let run_id = {
        let st = state.lock().await;
        st.span_id_hex()
    };
    if let Some(dir) = &checkpoint_dir {
        let open = build_open_record(&state).await;
        if let Err(e) = checkpoint::append_open(dir, &run_id, &open, cfg.tee_durable).await {
            // A failed checkpoint is a durability gap, not a fatal error: the
            // span still runs and exports normally. Surface it loudly.
            eprintln!("otel-cli bg: warning: checkpoint open failed: {e}");
        }
    }

    // Shutdown plumbing
    let shutdown = Arc::new(Notify::new());

    // Parent-pid watcher
    if !cfg.background_skip_parent_pid_check {
        let watcher_shutdown = shutdown.clone();
        let poll_ms = cfg.background_parent_poll_ms.max(1);
        spawn_parent_pid_watcher(poll_ms, watcher_shutdown);
    }

    // Signal watcher
    spawn_signal_watcher(shutdown.clone());

    // 3) accept loop
    let accept_state = state.clone();
    let accept_shutdown = shutdown.clone();
    let accept_loop = tokio::spawn(async move {
        loop {
            let accept_fut = listener.accept();
            tokio::select! {
                _ = accept_shutdown.notified() => break,
                res = accept_fut => {
                    match res {
                        Ok((stream, _addr)) => {
                            let st = accept_state.clone();
                            let sh = accept_shutdown.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_conn(stream, st, sh, recording).await {
                                    eprintln!("otel-cli bg: connection error: {e:#}");
                                }
                            });
                        }
                        Err(e) => {
                            eprintln!("otel-cli bg: accept error: {e}");
                            break;
                        }
                    }
                }
            }
        }
    });

    // 4) wait for shutdown
    shutdown.notified().await;

    // Best-effort cleanup of accept loop.
    accept_loop.abort();
    let _ = tokio::fs::remove_file(&sock_path).await;

    // 5) finalize span: set end time, send via the configured client.
    let mut rs = {
        let mut guard = state.lock().await;
        guard.ended = true;
        guard.rs.clone()
    };
    let now_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0).max(0) as u64;
    if let Some(span) = rs
        .scope_spans
        .get_mut(0)
        .and_then(|ss| ss.spans.get_mut(0))
    {
        if span.end_time_unix_nano == 0 || span.end_time_unix_nano < span.start_time_unix_nano {
            span.end_time_unix_nano = now_ns;
        }
    }

    // Append the `close` checkpoint before exporting: once a clean close is
    // durable, startup replay will pair it with the `open` and treat the span
    // as completed rather than an orphan.
    if let Some(dir) = &checkpoint_dir {
        let close = build_close_record(&rs);
        if let Err(e) = checkpoint::append_close(dir, &run_id, &close, cfg.tee_durable).await {
            eprintln!("otel-cli bg: warning: checkpoint close failed: {e}");
        }
    }

    let mut client = build_client(&cfg);
    // Bound the final drain so a SIGTERM/shutdown can't hang on an unreachable
    // collector (§9.3). The tee client drains both legs (network + file) inside
    // stop(); a 5s ceiling keeps shutdown prompt. The file leg + the durable
    // `close` checkpoint already written above are the durability backstop, so
    // a timeout is non-fatal unless --fail was requested.
    let export = async {
        client.start().await.context("client start")?;
        client
            .upload_traces(vec![rs])
            .await
            .context("upload traces")?;
        client.stop().await.context("client stop")?;
        Ok(())
    };
    let send: Result<()> = match tokio::time::timeout(Duration::from_secs(5), export).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("span export timed out after 5s")),
    };
    if let Err(e) = send {
        if cfg.fail {
            return Err(e);
        }
        eprintln!("otel-cli bg: warning: {e:#}");
    }

    Ok(())
}

/// Build an `open` checkpoint record from the live span state.
async fn build_open_record(state: &Arc<Mutex<BgState>>) -> CheckpointRecord {
    let st = state.lock().await;
    let span = &st.rs.scope_spans[0].spans[0];
    let service = st
        .rs
        .resource
        .as_ref()
        .and_then(|r| r.attributes.iter().find(|kv| kv.key == "service.name"))
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.as_ref())
        .map(|v| match v {
            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
            _ => "otel-cli".to_string(),
        })
        .unwrap_or_else(|| "otel-cli".to_string());

    let parent = if span.parent_span_id.is_empty() {
        None
    } else {
        Some(hex::encode(&span.parent_span_id))
    };

    CheckpointRecord::Open {
        trace_id: hex::encode(&span.trace_id),
        span_id: hex::encode(&span.span_id),
        parent_span_id: parent,
        name: span.name.clone(),
        service,
        start_unix_nano: span.start_time_unix_nano,
        attrs: keyvalues_to_strings(&span.attributes),
    }
}

/// Build a `close` checkpoint record from the finalized span.
fn build_close_record(rs: &ResourceSpans) -> CheckpointRecord {
    let span = &rs.scope_spans[0].spans[0];
    let status = span
        .status
        .as_ref()
        .map(|s| match s.code {
            c if c == opentelemetry_proto::tonic::trace::v1::status::StatusCode::Ok as i32 => {
                "ok".to_string()
            }
            c if c == opentelemetry_proto::tonic::trace::v1::status::StatusCode::Error as i32 => {
                "error".to_string()
            }
            _ => "unset".to_string(),
        })
        .unwrap_or_else(|| "unset".to_string());

    CheckpointRecord::Close {
        trace_id: hex::encode(&span.trace_id),
        span_id: hex::encode(&span.span_id),
        end_unix_nano: span.end_time_unix_nano,
        status,
        // Persist the FULL attribute set (open-time + end-time merged) so
        // orphan-replay reconstructs spans with their final attributes.
        // Previously this was empty, and replayed spans dropped every attr
        // the caller had attached.
        attrs: keyvalues_to_strings(&span.attributes),
    }
}

/// Flatten a span's `KeyValue` attributes back to a string map for the
/// checkpoint record. Only scalar values are preserved; complex values fall
/// back to their JSON-ish debug form (rare for CLI-supplied attrs).
fn keyvalues_to_strings(kvs: &[KeyValue]) -> std::collections::BTreeMap<String, String> {
    use opentelemetry_proto::tonic::common::v1::any_value::Value as V;
    let mut out = std::collections::BTreeMap::new();
    for kv in kvs {
        let v = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(V::StringValue(s)) => s.clone(),
            Some(V::IntValue(i)) => i.to_string(),
            Some(V::DoubleValue(d)) => d.to_string(),
            Some(V::BoolValue(b)) => b.to_string(),
            _ => continue,
        };
        out.insert(kv.key.clone(), v);
    }
    out
}

#[cfg(unix)]
fn spawn_parent_pid_watcher(poll_ms: u64, shutdown: Arc<Notify>) {
    // The foreground forwarder passes its own parent PID via this env var so
    // the detached child watches the shell, not the (already-exited)
    // forwarder. Falls back to `getppid()` when invoked directly without the
    // forwarder (e.g. unit tests).
    let watch_pid = std::env::var("OTEL_CLI_BG_WATCH_PPID")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or_else(|| nix::unistd::getppid().as_raw());

    tokio::spawn(async move {
        let interval = Duration::from_millis(poll_ms);
        // Give the foreground a moment to actually exit so the test below
        // doesn't race against a still-alive forwarder.
        tokio::time::sleep(interval).await;
        loop {
            // `kill(pid, 0)` returns Ok if the process is still alive.
            let alive = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(watch_pid),
                None,
            )
            .is_ok();
            if !alive {
                shutdown.notify_waiters();
                return;
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(unix)]
fn spawn_signal_watcher(shutdown: Arc<Notify>) {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("otel-cli bg: install SIGINT failed: {e}");
                return;
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("otel-cli bg: install SIGTERM failed: {e}");
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        shutdown.notify_waiters();
    });
}

async fn handle_conn(
    stream: UnixStream,
    state: Arc<Mutex<BgState>>,
    shutdown: Arc<Notify>,
    recording: bool,
) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    // One request per connection — Go's net/rpc keeps the conn open for many
    // calls but for our simpler use case each subcommand makes one call.
    let req: Request = read_frame(&mut reader).await.context("reading request")?;

    let resp = match req {
        Request::Wait => {
            let st = state.lock().await;
            Response::Ok {
                trace_id: st.trace_id_hex(),
                span_id: st.span_id_hex(),
                traceparent: st.traceparent(recording),
            }
        }
        Request::AddEvent { name, time, attrs } => {
            let now = Utc::now();
            let ts = parse_cli_time(&time).unwrap_or(now);
            let ts_ns = ts.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;

            let event = Event {
                time_unix_nano: ts_ns,
                name,
                attributes: kv_from_btree(&attrs),
                dropped_attributes_count: 0,
            };
            let mut st = state.lock().await;
            if !st.ended {
                if let Some(span) = st
                    .rs
                    .scope_spans
                    .get_mut(0)
                    .and_then(|ss| ss.spans.get_mut(0))
                {
                    span.events.push(event);
                }
            }
            Response::Ok {
                trace_id: st.trace_id_hex(),
                span_id: st.span_id_hex(),
                traceparent: st.traceparent(recording),
            }
        }
        Request::End {
            time,
            attrs,
            status_code,
            status_description,
        } => {
            let end_ts = time
                .as_deref()
                .and_then(|t| parse_cli_time(t).ok())
                .unwrap_or_else(Utc::now);
            let end_ns = end_ts.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;

            let mut st = state.lock().await;
            if let Some(span) = st
                .rs
                .scope_spans
                .get_mut(0)
                .and_then(|ss| ss.spans.get_mut(0))
            {
                span.end_time_unix_nano = end_ns;
                if !attrs.is_empty() {
                    // Merge: overlay attrs over the existing ones. Keep
                    // existing order, append unseen keys.
                    let extra = kv_from_btree(&attrs);
                    for new_kv in extra {
                        if let Some(existing) =
                            span.attributes.iter_mut().find(|kv| kv.key == new_kv.key)
                        {
                            existing.value = new_kv.value;
                        } else {
                            span.attributes.push(new_kv);
                        }
                    }
                }
                if let Some(code) = status_code.as_deref() {
                    let parsed = status_code_from_str(code);
                    let message = status_description.clone().unwrap_or_default();
                    span.status = Some(Status {
                        message,
                        code: parsed as i32,
                    });
                }
            }
            let resp = Response::Ok {
                trace_id: st.trace_id_hex(),
                span_id: st.span_id_hex(),
                traceparent: st.traceparent(recording),
            };
            st.ended = true;
            drop(st);

            write_frame(&mut writer, &resp).await.context("writing response")?;
            writer.flush().await.context("flush response")?;
            // ack delivered — trigger graceful shutdown
            shutdown.notify_waiters();
            return Ok(());
        }
    };

    write_frame(&mut writer, &resp).await.context("writing response")?;
    writer.flush().await.context("flush response")?;
    // Half-close so the client gets EOF immediately.
    let _ = writer.shutdown().await;
    Ok(())
}

fn kv_from_btree(attrs: &std::collections::BTreeMap<String, String>) -> Vec<KeyValue> {
    attrs_to_keyvalues(attrs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::protocol::Request;
    use std::collections::BTreeMap;
    use tempfile::TempDir;
    use tokio::time::timeout;

    fn test_cfg(tmp_json_dir: &Path) -> Config {
        let mut c = Config::defaults();
        c.service_name = "bg-test".to_string();
        c.span_name = "bg-span".to_string();
        c.kind = "internal".to_string();
        c.json_dir = tmp_json_dir.to_string_lossy().to_string();
        c.protocol = "json+file".to_string();
        c.traceparent_ignore_env = true;
        // Avoid the watcher tripping during the test (we don't fork a child).
        c.background_skip_parent_pid_check = true;
        c
    }

    async fn send_one(sock: &Path, req: &Request) -> Response {
        let stream = UnixStream::connect(sock).await.expect("connect");
        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);
        write_frame(&mut writer, req).await.expect("write");
        writer.flush().await.expect("flush");
        read_frame(&mut reader).await.expect("read")
    }

    #[tokio::test]
    async fn server_accepts_event_and_end_in_process() {
        let sockdir = TempDir::new().unwrap();
        let outdir = TempDir::new().unwrap();
        let cfg = test_cfg(outdir.path());
        let sock_path = socket_path(sockdir.path());

        let cfg_clone = cfg.clone();
        let sockdir_path = sockdir.path().to_path_buf();
        let server = tokio::spawn(async move { run_server(cfg_clone, sockdir_path).await });

        // Wait for socket to exist (server is async).
        for _ in 0..200 {
            if tokio::fs::metadata(&sock_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            tokio::fs::metadata(&sock_path).await.is_ok(),
            "socket {sock_path:?} should exist"
        );

        // AddEvent
        let resp = timeout(
            Duration::from_secs(5),
            send_one(
                &sock_path,
                &Request::AddEvent {
                    name: "evt".to_string(),
                    time: "now".to_string(),
                    attrs: BTreeMap::new(),
                },
            ),
        )
        .await
        .expect("addevent timeout");
        match resp {
            Response::Ok { .. } => {}
            Response::Err { message } => panic!("AddEvent err: {message}"),
        }

        // End
        let resp = timeout(
            Duration::from_secs(5),
            send_one(
                &sock_path,
                &Request::End {
                    time: None,
                    attrs: BTreeMap::new(),
                    status_code: None,
                    status_description: None,
                },
            ),
        )
        .await
        .expect("end timeout");
        match resp {
            Response::Ok { .. } => {}
            Response::Err { message } => panic!("End err: {message}"),
        }

        // Server should exit cleanly.
        let result = timeout(Duration::from_secs(5), server).await;
        match result {
            Ok(Ok(Ok(()))) => {}
            other => panic!("server did not exit cleanly: {other:?}"),
        }

        // Socket file removed
        assert!(
            tokio::fs::metadata(&sock_path).await.is_err(),
            "socket file should be removed after shutdown"
        );

        // Verify span.json + event-0.json were written by json+file client.
        let mut found_span = false;
        let mut found_event = false;
        for entry in walkdir::WalkDir::new(outdir.path()) {
            let e = entry.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "span.json" {
                found_span = true;
            }
            if name == "event-0.json" {
                found_event = true;
            }
        }
        assert!(found_span, "span.json should be written");
        assert!(found_event, "event-0.json should be written");
    }

    #[tokio::test]
    async fn wait_request_returns_immediately() {
        let sockdir = TempDir::new().unwrap();
        let outdir = TempDir::new().unwrap();
        let cfg = test_cfg(outdir.path());
        let sock_path = socket_path(sockdir.path());

        let cfg_clone = cfg.clone();
        let sockdir_path = sockdir.path().to_path_buf();
        let server = tokio::spawn(async move { run_server(cfg_clone, sockdir_path).await });

        for _ in 0..200 {
            if tokio::fs::metadata(&sock_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let resp = timeout(Duration::from_secs(5), send_one(&sock_path, &Request::Wait))
            .await
            .expect("wait timeout");
        assert!(matches!(resp, Response::Ok { .. }));

        // Now end so the server exits.
        let _ = timeout(
            Duration::from_secs(5),
            send_one(
                &sock_path,
                &Request::End {
                    time: None,
                    attrs: BTreeMap::new(),
                    status_code: None,
                    status_description: None,
                },
            ),
        )
        .await
        .expect("end timeout");

        let _ = timeout(Duration::from_secs(5), server).await;
    }
}
