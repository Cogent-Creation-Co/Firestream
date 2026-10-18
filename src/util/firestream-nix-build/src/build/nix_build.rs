//! Spawn `nix build` for a single derivation, optionally with stderr tee'd
//! into nom + the in-process OTel ingest. Mirrors `nix_build()` at
//! `__init__.py:1089-1175` plus the per-build forwarding loop at `:1146-1155`.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::build::log::get_build_log;
use crate::build::{Build, BuildResult};
use crate::display::RingFormatter;
use crate::nom::NomPipe;
use crate::options::{Options, maybe_remote};
use crate::otel::PerBuildIngest;

/// Run one `nix build` for `build.attr`. When `nom_pipe` or `otel_ingest` is
/// active, nix is launched with `--log-format internal-json -v` and stderr
/// is tee'd into both consumers.
pub async fn run_nix_build(
    build: &Build,
    installable: &str,
    opts: &Options,
    nom_pipe: Option<&NomPipe>,
    otel: Option<&PerBuildIngest>,
) -> Result<BuildResult> {
    let mut args = opts.nix_build_command(&[&format!("{installable}^*"), "--keep-going"]);
    args.extend(opts.options.iter().cloned());

    // Whether we need to read stderr in-process. nom and otel always
    // require it; the new `opts.stderr_log` knob does too. The
    // `--log-format internal-json -v` flags are only needed for
    // nom/otel — if the caller only wants raw stderr in a log file, leave
    // nix's stderr in its default human-readable format.
    let need_json_stderr = nom_pipe.is_some() || otel.is_some();
    let capture = need_json_stderr || opts.stderr_log.is_some();
    if need_json_stderr {
        args.push("--log-format".into());
        args.push("internal-json".into());
        args.push("-v".into());
    }
    if opts.no_link {
        args.push("--no-link".into());
    } else {
        args.push("--out-link".into());
        args.push(format!("{}-{}", opts.out_link, build.attr));
    }
    let args = maybe_remote(args, opts);
    tracing::debug!(
        "run {}",
        shlex::try_join(args.iter().map(String::as_str)).unwrap_or_default()
    );

    let stderr_stdio = if capture {
        Stdio::piped()
    } else {
        Stdio::inherit()
    };
    let mut child: Child = Command::new(&args[0])
        .args(&args[1..])
        .stderr(stderr_stdio)
        .spawn()
        .with_context(|| format!("spawn {}", args[0]))?;

    if capture {
        // Tee stderr into nom (via dup of write_fd), otel (in-process),
        // and the caller's stderr_log file (if any).
        let stderr = child.stderr.take().expect("stderr piped");
        let mut reader = BufReader::new(stderr).lines();

        #[cfg(unix)]
        let mut nom_writer = nom_pipe
            .map(|p| -> Result<tokio::fs::File> {
                let dup = nix::unistd::dup(p.write_fd.as_raw_fd()).context("dup nom write fd")?;
                // SAFETY: `dup` returned a fresh fd we exclusively own.
                let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(dup) };
                Ok(tokio::fs::File::from_std(std::fs::File::from(owned)))
            })
            .transpose()?;
        #[cfg(not(unix))]
        let mut nom_writer: Option<tokio::fs::File> = None;

        let mut otel_state = otel.map(|i| i.fresh_state());

        // Optional per-attr stderr log. Append mode so concurrent calls
        // for the *same* path (unusual but possible) don't truncate.
        let mut stderr_log_writer: Option<tokio::fs::File> = match &opts.stderr_log {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await
                {
                    Ok(f) => Some(f),
                    Err(e) => {
                        tracing::warn!("open stderr_log {}: {e}", path.display());
                        None
                    }
                }
            }
            None => None,
        };

        // Per-build humanizer for the dashboard ring. Keeps raw bytes flowing
        // to nom / OTel / stderr_log; only the *dashboard tail* gets the
        // human-readable projection.
        let mut ring_fmt = opts.ring_sink.as_ref().map(|_| RingFormatter::new());

        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    if let Some(w) = nom_writer.as_mut() {
                        use tokio::io::AsyncWriteExt;
                        let _ = w.write_all(line.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        let _ = w.flush().await;
                    }
                    if let (Some(otel_ref), Some(st)) = (otel, otel_state.as_mut()) {
                        otel_ref.feed_line(st, &line).await;
                    }
                    if let Some(w) = stderr_log_writer.as_mut() {
                        use tokio::io::AsyncWriteExt;
                        let _ = w.write_all(line.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                    }
                    if let (Some(ring), Some(fmt)) =
                        (opts.ring_sink.as_ref(), ring_fmt.as_mut())
                    {
                        if let Some(pretty) = fmt.humanize(line) {
                            ring.push(pretty);
                        }
                    }
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    tracing::warn!("stderr read error: {e}");
                    break;
                }
            }
        }

        if let Some(w) = stderr_log_writer.as_mut() {
            use tokio::io::AsyncWriteExt;
            let _ = w.flush().await;
        }

        if let (Some(otel_ref), Some(st)) = (otel, otel_state) {
            otel_ref.finish(st).await;
        }
    }

    // Wait for the build to finish.
    //
    // `opts.retries` is accepted for CLI compatibility but not yet honoured:
    // the upstream Python tool documents `--retries` as "retry failed builds"
    // but only re-awaits the same (already-exited) child, which returns the
    // same code. We mirror that single-await semantics exactly — a real
    // respawn-on-failure retry path is out of scope for the initial port.
    let status = child.wait().await.context("wait for nix build")?;
    let rc = status.code().unwrap_or(-1);
    if rc == 0 {
        return Ok(BuildResult {
            return_code: 0,
            log_output: String::new(),
        });
    }
    tracing::warn!("build {} exited with {rc}", build.attr);

    let log_output = get_build_log(&build.drv_path, opts).await?;
    Ok(BuildResult {
        return_code: rc,
        log_output,
    })
}
