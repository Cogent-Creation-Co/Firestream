//! Pattern #2 — instrumented child process. Mirrors
//! `bin/_lib.sh::run_with_log` (lines 134-148) + `describe_exit` (151-159).
//!
//! Each invocation opens a span via `trace::Tracer`, captures stdout +
//! stderr to per-stream log paths (or discards / buffers in memory),
//! preserves the child's exit code, and annotates the result with OOM /
//! signal info via `util::describe_exit`.
//!
//! The bash spec tees stdout+stderr together to a single log. We split
//! into two files because (a) it matches what real CI consumers want
//! (Honeycomb reads stderr separately for error spans), and (b) the join
//! is a trivial post-hoc concat if needed. The bash is the spec for
//! exit-code semantics, not for log layout.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex as AsyncMutex;

use crate::trace::{Span, SpanStatus};
use crate::util::{describe_exit, signal_from_exit};

#[derive(Debug, Error)]
pub enum Error {
    #[error("exec: spawn failed for `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("exec: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("exec: child exited with timeout-kill after {0:?}")]
    Timeout(Duration),
}

/// Where stdout / stderr should land.
#[derive(Debug, Clone)]
pub enum LogSink {
    /// Write to a file in the rundir's `logs/` subdir. The wrapper opens
    /// the file with O_APPEND so reruns of the same command-id append
    /// rather than truncate.
    File(PathBuf),
    /// Append to `path` line-buffered AND mirror each line to the
    /// process's stderr with `prefix` prepended. Empty `prefix` writes
    /// the line through verbatim. Concurrent tasks serialize at line
    /// granularity via a process-global mutex (no torn output).
    TeeTerminal { path: PathBuf, prefix: String },
    /// Capture into the `ExitReport` body field.
    Buffer,
    /// Drop the stream entirely.
    Discard,
}

#[derive(Debug, Clone, Default)]
pub struct ExitReport {
    pub exit_code: i32,
    pub duration: Duration,
    pub signal: Option<i32>,
    pub oom_suspected: bool,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
    pub stdout_buf: Option<Vec<u8>>,
    pub stderr_buf: Option<Vec<u8>>,
    pub code_description: &'static str,
}

impl ExitReport {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Builder for an instrumented child process. Wraps `tokio::process::Command`.
pub struct Command {
    program: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    stdout_sink: LogSink,
    stderr_sink: LogSink,
    span_name: Option<String>,
    timeout: Option<Duration>,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            cwd: None,
            stdout_sink: LogSink::Buffer,
            stderr_sink: LogSink::Buffer,
            span_name: None,
            timeout: None,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.envs.push((k.into(), v.into()));
        self
    }

    pub fn envs<I, K, V>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in pairs {
            self.envs.push((k.into(), v.into()));
        }
        self
    }

    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
        self.cwd = Some(p.into());
        self
    }

    pub fn stdout(mut self, sink: LogSink) -> Self {
        self.stdout_sink = sink;
        self
    }

    pub fn stderr(mut self, sink: LogSink) -> Self {
        self.stderr_sink = sink;
        self
    }

    /// Route both stdout and stderr to a single merged log file with each
    /// line also mirrored to the process's stderr, prefixed with
    /// `[<prefix>] `. Concurrent tasks serialize at line granularity so
    /// output stays readable when many tasks run in parallel.
    pub fn tee_terminal(mut self, prefix: impl Into<String>, path: PathBuf) -> Self {
        let prefix = prefix.into();
        let sink = LogSink::TeeTerminal { path, prefix };
        self.stdout_sink = sink.clone();
        self.stderr_sink = sink;
        self
    }

    pub fn span(mut self, name: impl Into<String>) -> Self {
        self.span_name = Some(name.into());
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// Replace `program` and shift the original `program`+`args` to the
    /// end of `prefix`. Used by `limits::LimitedCommand` to inject a
    /// `systemd-run --user --scope … --` argv prefix without forcing the
    /// caller to know about that wrapping. Env, cwd, sinks, span name,
    /// and timeout are preserved untouched.
    pub fn with_program_prefix(mut self, prefix: Vec<String>) -> Self {
        if prefix.is_empty() {
            return self;
        }
        let mut iter = prefix.into_iter();
        let new_program = iter.next().expect("prefix non-empty");
        let mut new_args: Vec<String> = iter.collect();
        new_args.push(std::mem::replace(&mut self.program, new_program));
        new_args.extend(std::mem::take(&mut self.args));
        self.args = new_args;
        self
    }

    /// Debug-only view of the resolved argv (`program` + `args`). Useful
    /// for tests that need to assert how a wrapper rewrote the command
    /// before spawn.
    pub fn argv_for_debug(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.args.len() + 1);
        out.push(self.program.clone());
        out.extend(self.args.iter().cloned());
        out
    }

    /// Spawn under a parent span; child inherits the parent's traceparent.
    /// Returns a typed `ExitReport` with span status set from the exit code.
    pub async fn run(self, parent: &Span) -> Result<ExitReport, Error> {
        let mut child_span = parent.child(
            self.span_name
                .clone()
                .unwrap_or_else(|| self.program.clone()),
        );
        let report = self.run_inner(&parent.traceparent_env()).await;
        // Set span status from the exit; treat timeout as Error and OOM as Error.
        match &report {
            Ok(r) if r.success() => child_span.set_status(SpanStatus::Ok),
            Ok(r) => {
                child_span.set_status(SpanStatus::Error);
                child_span
                    .set_status_message(format!("{} (exit {})", r.code_description, r.exit_code));
                child_span.set_attribute("exec.exit_code", r.exit_code.to_string());
                if let Some(sig) = r.signal {
                    child_span.set_attribute("exec.signal", sig.to_string());
                }
                if r.oom_suspected {
                    child_span.set_attribute("exec.oom_suspected", "true");
                }
            }
            Err(_) => {
                child_span.set_status(SpanStatus::Error);
                child_span.set_status_message("spawn or I/O failed");
            }
        }
        report
    }

    /// Spawn without a parent span. Useful when the caller already owns
    /// the tracing context (e.g. logs via `tracing::info!`) and just wants
    /// the typed exit report.
    pub async fn run_unsupervised(self) -> Result<ExitReport, Error> {
        self.run_inner(&[]).await
    }

    async fn run_inner(self, parent_env: &[(String, String)]) -> Result<ExitReport, Error> {
        let started = Instant::now();
        let mut cmd = TokioCommand::new(&self.program);
        cmd.args(&self.args);
        for (k, v) in parent_env {
            cmd.env(k, v);
        }
        for (k, v) in &self.envs {
            cmd.env(k, v);
        }
        if let Some(d) = &self.cwd {
            cmd.current_dir(d);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(|source| Error::Spawn {
            program: self.program.clone(),
            source,
        })?;

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let stdout_task = tokio::spawn(drain_to_sink(stdout_handle, self.stdout_sink.clone()));
        let stderr_task = tokio::spawn(drain_to_sink(stderr_handle, self.stderr_sink.clone()));

        let wait_status = match self.timeout {
            Some(d) => match tokio::time::timeout(d, child.wait()).await {
                Ok(s) => s,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    return Err(Error::Timeout(d));
                }
            },
            None => child.wait().await,
        }
        .map_err(|source| Error::Io {
            path: PathBuf::from(&self.program),
            source,
        })?;

        let stdout_result = stdout_task.await.map_err(|e| Error::Io {
            path: PathBuf::from("<stdout-task>"),
            source: std::io::Error::other(e),
        })??;
        let stderr_result = stderr_task.await.map_err(|e| Error::Io {
            path: PathBuf::from("<stderr-task>"),
            source: std::io::Error::other(e),
        })??;

        let exit_code = wait_status.code().unwrap_or_else(|| {
            // Killed by signal: synthesize a POSIX shell-style 128+signum.
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                wait_status.signal().map(|s| 128 + s).unwrap_or(-1)
            }
            #[cfg(not(unix))]
            {
                -1
            }
        });

        let signal = signal_from_exit(exit_code);
        // OOM = killed by SIGKILL specifically, which exits with 137.
        // The bash check (`run_with_log` line 148) is the same.
        let oom_suspected = exit_code == 137;

        let (stdout_path, stdout_buf) = stdout_result.into_parts();
        let (stderr_path, stderr_buf) = stderr_result.into_parts();

        Ok(ExitReport {
            exit_code,
            duration: started.elapsed(),
            signal,
            oom_suspected,
            stdout_path,
            stderr_path,
            stdout_buf,
            stderr_buf,
            code_description: describe_exit(exit_code),
        })
    }
}

struct SinkResult {
    file_path: Option<PathBuf>,
    buffer: Option<Vec<u8>>,
}

impl SinkResult {
    fn into_parts(self) -> (Option<PathBuf>, Option<Vec<u8>>) {
        (self.file_path, self.buffer)
    }
}

async fn drain_to_sink<R>(stream: Option<R>, sink: LogSink) -> Result<SinkResult, Error>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let stream = match stream {
        Some(s) => s,
        None => {
            return Ok(SinkResult {
                file_path: None,
                buffer: None,
            });
        }
    };
    let mut reader = BufReader::new(stream);

    match sink {
        LogSink::File(path) => {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| Error::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
            }
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            let mut line = Vec::with_capacity(256);
            loop {
                line.clear();
                let n = reader
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })?;
                if n == 0 {
                    break;
                }
                f.write_all(&line).await.map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            }
            f.flush().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            Ok(SinkResult {
                file_path: Some(path),
                buffer: None,
            })
        }
        LogSink::TeeTerminal { path, prefix } => {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| Error::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
            }
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            // Pre-render the prefix bytes once: `[<prefix>] ` (or empty).
            let prefix_bytes: Vec<u8> = if prefix.is_empty() {
                Vec::new()
            } else {
                format!("[{prefix}] ").into_bytes()
            };
            let stderr_lock = terminal_stderr_lock();
            let mut line = Vec::with_capacity(256);
            loop {
                line.clear();
                let n = reader
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })?;
                if n == 0 {
                    break;
                }
                // File: raw bytes, no prefix (post-hoc readability beats
                // per-line prefix in the persisted log).
                f.write_all(&line).await.map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
                // Drop known-harmless host-nix-conf warnings from the
                // terminal mirror only. The full unfiltered line is still
                // in the persisted log above. Each parallel nix invocation
                // re-emits these because the running user isn't the daemon;
                // fixing it requires editing the user's /etc/nix/nix.conf.
                if is_noisy_nix_warning(&line) {
                    continue;
                }
                // When a TUI owns the terminal, skip the stderr mirror
                // entirely — any write here would mangle its redraws.
                // The full line is already in the persisted log above.
                if dashboard_owns_terminal() {
                    continue;
                }
                // Terminal: prefix + line, serialized so parallel tasks
                // don't tear. Mutex held only across one logical write.
                let _g = stderr_lock.lock().await;
                let mut err = tokio::io::stderr();
                if !prefix_bytes.is_empty() {
                    let _ = err.write_all(&prefix_bytes).await;
                }
                let _ = err.write_all(&line).await;
                let _ = err.flush().await;
            }
            f.flush().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            Ok(SinkResult {
                file_path: Some(path),
                buffer: None,
            })
        }
        LogSink::Buffer => {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            reader
                .read_to_end(&mut buf)
                .await
                .map_err(|source| Error::Io {
                    path: PathBuf::from("<buffer>"),
                    source,
                })?;
            Ok(SinkResult {
                file_path: None,
                buffer: Some(buf),
            })
        }
        LogSink::Discard => {
            let mut sink = tokio::io::sink();
            tokio::io::copy(&mut reader, &mut sink)
                .await
                .map_err(|source| Error::Io {
                    path: PathBuf::from("<discard>"),
                    source,
                })?;
            Ok(SinkResult {
                file_path: None,
                buffer: None,
            })
        }
    }
}

/// Process-global serialization point for writes from `LogSink::TeeTerminal`.
/// Ensures concurrent tasks don't tear each other's lines on the terminal.
fn terminal_stderr_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// When a TUI (e.g. the dashboard reporter) owns the terminal, any other
/// writer to stderr will mangle its redraws. The TUI flips this on at
/// startup so `LogSink::TeeTerminal` writes still hit the persisted log
/// file but skip the stderr mirror.
static DASHBOARD_OWNS_TERMINAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called by the dashboard reporter once it's selected and started.
pub fn set_dashboard_owns_terminal(yes: bool) {
    DASHBOARD_OWNS_TERMINAL.store(yes, std::sync::atomic::Ordering::Release);
}

fn dashboard_owns_terminal() -> bool {
    DASHBOARD_OWNS_TERMINAL.load(std::sync::atomic::Ordering::Acquire)
}

/// Recognize host-nix-conf warnings that re-emit once per parallel task and
/// carry no actionable signal. `allowed-users` / `trusted-users` only apply
/// to the nix daemon; on hosts where they're set in `/etc/nix/nix.conf`,
/// every user-mode child nix process warns about them.
fn is_noisy_nix_warning(line: &[u8]) -> bool {
    const PATTERNS: &[&[u8]] = &[
        b"warning: unknown setting 'allowed-users'",
        b"warning: unknown setting 'trusted-users'",
    ];
    // Anchor at start of (possibly ANSI-prefixed) line. The bytes we care
    // about may have escape codes around them — search rather than prefix.
    PATTERNS.iter().any(|p| memmem_contains(line, p))
}

fn memmem_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return needle.is_empty();
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Tracer;
    use tempfile::tempdir;

    async fn quiet_tracer() -> Tracer {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        Tracer::builder()
            .service("firestream-ci-test")
            .root_only()
            .build()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn captures_stdout_to_buffer() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let report = Command::new("sh")
            .args(["-c", "echo hello"])
            .span("echo")
            .run(&parent)
            .await
            .unwrap();
        assert!(report.success());
        assert_eq!(report.exit_code, 0);
        let out = report.stdout_buf.unwrap();
        assert_eq!(String::from_utf8(out).unwrap().trim(), "hello");
    }

    #[tokio::test]
    async fn preserves_non_zero_exit() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let report = Command::new("sh")
            .args(["-c", "exit 3"])
            .run(&parent)
            .await
            .unwrap();
        assert!(!report.success());
        assert_eq!(report.exit_code, 3);
        assert_eq!(report.code_description, "non-zero exit");
        assert!(!report.oom_suspected);
    }

    #[tokio::test]
    async fn flags_oom_on_137() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let report = Command::new("sh")
            .args(["-c", "exit 137"])
            .run(&parent)
            .await
            .unwrap();
        assert_eq!(report.exit_code, 137);
        assert!(report.oom_suspected);
        assert_eq!(report.code_description, "OOM killed (SIGKILL)");
    }

    #[tokio::test]
    async fn writes_stdout_to_file_sink() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("stdout.log");
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let report = Command::new("sh")
            .args(["-c", "echo line1; echo line2"])
            .stdout(LogSink::File(log.clone()))
            .run(&parent)
            .await
            .unwrap();
        assert!(report.success());
        let body = std::fs::read_to_string(&log).unwrap();
        assert!(body.contains("line1"));
        assert!(body.contains("line2"));
        assert_eq!(report.stdout_path.unwrap(), log);
    }

    #[tokio::test]
    async fn injects_traceparent_into_child_env() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let report = Command::new("sh")
            .args(["-c", "echo $TRACEPARENT"])
            .run(&parent)
            .await
            .unwrap();
        let out = String::from_utf8(report.stdout_buf.unwrap()).unwrap();
        assert!(
            out.contains("00-"),
            "child did not see TRACEPARENT in env: `{out}`"
        );
    }

    #[tokio::test]
    async fn timeout_kills_child() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let err = Command::new("sh")
            .args(["-c", "sleep 5"])
            .timeout(Duration::from_millis(100))
            .run(&parent)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Timeout(_)));
    }

    /// Tee sink writes raw bytes (no prefix) to its log file.
    #[tokio::test]
    async fn tee_terminal_writes_file() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("tee.log");
        let report = Command::new("sh")
            .args(["-c", "echo first; echo second"])
            .tee_terminal("test", log.clone())
            .run_unsupervised()
            .await
            .unwrap();
        assert!(report.success());
        let body = std::fs::read_to_string(&log).unwrap();
        assert!(body.contains("first\n"), "log missing first: {body}");
        assert!(body.contains("second\n"), "log missing second: {body}");
        // The file is the raw stream — prefix is not written to it.
        assert!(!body.contains("[test]"), "prefix leaked into file: {body}");
    }

    /// Tee sink emits to its file as bytes arrive — not buffered until the
    /// child exits. Verified by checking the file mid-flight while the
    /// child is still alive in its sleep.
    #[tokio::test]
    async fn tee_terminal_streams_mid_flight() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("stream.log");
        let log_for_check = log.clone();
        // Spawn the command in the background, then poll the file.
        let run = tokio::spawn(async move {
            Command::new("sh")
                .args(["-c", "echo early; sleep 0.6; echo late"])
                .tee_terminal("stream", log)
                .run_unsupervised()
                .await
                .unwrap()
        });
        // Wait long enough for the first echo to land but well before the
        // sleep ends. 250ms is a generous middle ground.
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mid = std::fs::read_to_string(&log_for_check).unwrap_or_default();
        assert!(
            mid.contains("early\n"),
            "tee did not stream: file was {mid:?} at 250ms",
        );
        assert!(
            !mid.contains("late\n"),
            "child exited prematurely (or test timing too lax): {mid:?}",
        );
        let report = run.await.unwrap();
        assert!(report.success());
        let body = std::fs::read_to_string(&log_for_check).unwrap();
        assert!(body.contains("late\n"));
    }

    /// Two parallel tee sinks each writing many lines — both files should
    /// receive every one of their own lines verbatim (no torn output) and
    /// neither should mix in the other's lines.
    #[tokio::test]
    async fn tee_terminal_parallel_no_torn_lines() {
        let dir = tempdir().unwrap();
        let log_a = dir.path().join("a.log");
        let log_b = dir.path().join("b.log");

        let a = Command::new("sh")
            .args(["-c", "for i in $(seq 1 200); do echo a-$i; done"])
            .tee_terminal("a", log_a.clone())
            .run_unsupervised();
        let b = Command::new("sh")
            .args(["-c", "for i in $(seq 1 200); do echo b-$i; done"])
            .tee_terminal("b", log_b.clone())
            .run_unsupervised();
        let (ra, rb) = tokio::join!(a, b);
        assert!(ra.unwrap().success());
        assert!(rb.unwrap().success());

        let body_a = std::fs::read_to_string(&log_a).unwrap();
        let body_b = std::fs::read_to_string(&log_b).unwrap();
        for i in 1..=200 {
            assert!(body_a.contains(&format!("a-{i}\n")), "a missing a-{i}");
            assert!(body_b.contains(&format!("b-{i}\n")), "b missing b-{i}");
        }
        // Each log only owns its own stream.
        assert!(!body_a.contains("b-"), "a contaminated by b: {body_a:?}");
        assert!(!body_b.contains("a-"), "b contaminated by a: {body_b:?}");
    }
}
