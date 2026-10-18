//! Top-level orchestrator. Wires nix-eval-jobs → build queue → optional
//! upload/download/cachix/attic/niks3 queues, then drains and aggregates
//! `Outcome` records into the final exit code.
//!
//! Mirrors `run()` at `__init__.py:1688-1895`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::build::Build;
use crate::build::nix_build::run_nix_build;
use crate::ci_summary;
use crate::display::RingFormatter;
use crate::eval::{Job, RawEvalLine, is_noisy_eval_line, spawn_nix_eval_jobs};
use crate::nom::{NomPipe, spawn_nom};
use crate::options::{EvalMode, Options, ResultFormat};
use crate::otel::PerBuildIngest;
use crate::queue::{Sender, WorkQueue};
use crate::remote::{RemoteTempDir, upload_sources};
use crate::result::{Outcome, ResultKind, dump_json, dump_junit_xml};
use crate::ring::LineRing;
use crate::stop::ensure_stop;
use crate::upload;

type Outcomes = Arc<Mutex<Vec<Outcome>>>;

enum WorkItem<T> {
    Item(T),
    Stop,
}

pub async fn run(mut opts: Options) -> Result<u8> {
    // Source upload happens before anything else so the rest of the pipeline
    // can use the resolved flake URL on the remote builder.
    if opts.remote.is_some() && opts.eval_mode == EvalMode::Flake {
        opts.flake_url = upload_sources(&opts).await?;
    }

    // Working directory: either a local tempdir or a remote mktemp -d.
    let (_local_td, _remote_td, tmp_dir): (
        Option<tempfile::TempDir>,
        Option<RemoteTempDir>,
        PathBuf,
    ) = if opts.remote.is_some() {
        let td = RemoteTempDir::create(&opts).await?;
        let path = td.path.clone();
        (None, Some(td), path)
    } else {
        let td = tempfile::tempdir().context("create local tempdir")?;
        let path = td.path().to_path_buf();
        (Some(td), None, path)
    };

    let mut eval_proc = spawn_nix_eval_jobs(&tmp_dir, &opts).await?;
    let eval_stdout = eval_proc.stdout.take().context("eval stdout missing")?;
    // When `spawn_nix_eval_jobs` piped stderr (set when stderr_log or
    // ring_sink is configured), drain it here so the buffer never wedges.
    // The drain task tees to the same stderr_log file and ring sink as
    // the build-stage capture in `nix_build.rs`, dropping noise lines
    // (`unknown setting 'allowed-users'`, sqlite-busy) that re-emit per
    // parallel task and carry no signal.
    let eval_stderr_drain: Option<tokio::task::JoinHandle<()>> =
        if let Some(eval_stderr) = eval_proc.stderr.take() {
            let stderr_log = opts.stderr_log.clone();
            let ring = opts.ring_sink.clone();
            Some(tokio::spawn(async move {
                drain_eval_stderr(eval_stderr, stderr_log, ring).await
            }))
        } else {
            None
        };

    // Optional nom pipe + child.
    let nom_pipe: Option<Arc<NomPipe>> = if opts.nom {
        Some(Arc::new(NomPipe::new()?))
    } else {
        None
    };
    let mut nom_child = if let Some(p) = nom_pipe.as_ref() {
        Some(spawn_nom(p, &opts).await?)
    } else {
        None
    };

    // Optional cachix daemon.
    let cachix_daemon = if let Some(cache) = &opts.cachix_cache {
        Some(upload::cachix::CachixDaemon::start(&tmp_dir, cache, &opts).await?)
    } else {
        None
    };

    let outcomes: Outcomes = Arc::new(Mutex::new(Vec::new()));

    // Queues. Capacity matches the Python `Queue()` default (unbounded —
    // we pick a comfortable bound here that won't backpressure typical CI
    // runs).
    let build_q: Arc<WorkQueue<WorkItem<Job>>> = WorkQueue::new(1024);
    let upload_q: Option<Arc<WorkQueue<WorkItem<Build>>>> =
        opts.copy_to.as_ref().map(|_| WorkQueue::new(1024));
    let cachix_q: Option<Arc<WorkQueue<WorkItem<Build>>>> =
        cachix_daemon.as_ref().map(|_| WorkQueue::new(1024));
    let attic_q: Option<Arc<WorkQueue<WorkItem<Build>>>> =
        opts.attic_cache.as_ref().map(|_| WorkQueue::new(1024));
    let niks3_q: Option<Arc<WorkQueue<WorkItem<Build>>>> =
        opts.niks3_server.as_ref().map(|_| WorkQueue::new(1024));
    let download_q: Option<Arc<WorkQueue<WorkItem<Build>>>> =
        if opts.remote_url().is_some() && opts.download {
            Some(WorkQueue::new(1024))
        } else {
            None
        };

    // Process-wide OTel ingest gate (None if --otel-ingest is off).
    let otel = PerBuildIngest::from_options(&opts).map(Arc::new);

    let opts = Arc::new(opts);
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();

    // Evaluation task.
    {
        let opts = Arc::clone(&opts);
        let outcomes = Arc::clone(&outcomes);
        let build_tx = build_q.sender();
        let upload_tx = upload_q.as_ref().map(|q| q.sender());
        tasks.spawn(async move {
            run_evaluation(eval_stdout, build_tx, upload_tx, outcomes, &opts).await
        });
    }

    // N build workers.
    let max_jobs = opts.max_jobs.max(1);
    for i in 0..max_jobs {
        let opts = Arc::clone(&opts);
        let outcomes = Arc::clone(&outcomes);
        let build_q = Arc::clone(&build_q);
        let nom_pipe = nom_pipe.as_ref().map(Arc::clone);
        let otel = otel.as_ref().map(Arc::clone);
        let next_queues: Vec<Sender<WorkItem<Build>>> =
            [&upload_q, &cachix_q, &attic_q, &download_q, &niks3_q]
                .iter()
                .filter_map(|q| q.as_ref().map(|qq| qq.sender()))
                .collect();
        tasks.spawn(async move {
            run_build_worker(i, build_q, next_queues, nom_pipe, otel, outcomes, opts).await
        });
    }

    // Optional upload/cachix/attic/download workers (N each, matching max_jobs).
    if let Some(q) = upload_q.as_ref() {
        for _ in 0..max_jobs {
            let opts = Arc::clone(&opts);
            let outcomes = Arc::clone(&outcomes);
            let q = Arc::clone(q);
            tasks.spawn(async move { run_upload_worker(q, outcomes, opts).await });
        }
    }
    if let (Some(q), Some(d)) = (cachix_q.as_ref(), cachix_daemon.as_ref()) {
        for _ in 0..max_jobs {
            let opts = Arc::clone(&opts);
            let outcomes = Arc::clone(&outcomes);
            let q = Arc::clone(q);
            let sock = d.socket_path.clone();
            tasks.spawn(async move { run_cachix_worker(q, sock, outcomes, opts).await });
        }
    }
    if let Some(q) = attic_q.as_ref() {
        for _ in 0..max_jobs {
            let opts = Arc::clone(&opts);
            let outcomes = Arc::clone(&outcomes);
            let q = Arc::clone(q);
            tasks.spawn(async move { run_attic_worker(q, outcomes, opts).await });
        }
    }
    if let Some(q) = download_q.as_ref() {
        for _ in 0..max_jobs {
            let opts = Arc::clone(&opts);
            let outcomes = Arc::clone(&outcomes);
            let q = Arc::clone(q);
            tasks.spawn(async move { run_download_worker(q, outcomes, opts).await });
        }
    }
    // Single niks3 worker (it batches internally).
    if let Some(q) = niks3_q.as_ref() {
        let opts = Arc::clone(&opts);
        let outcomes = Arc::clone(&outcomes);
        let q = Arc::clone(q);
        tasks.spawn(async move { run_niks3_worker(q, outcomes, opts).await });
    }

    // Progress reporter when nom isn't claiming the terminal.
    let progress_handle = if !opts.nom {
        let build_q = Arc::clone(&build_q);
        let upload_q = upload_q.as_ref().map(Arc::clone);
        let download_q = download_q.as_ref().map(Arc::clone);
        Some(tokio::spawn(async move {
            report_progress(build_q, upload_q, download_q).await;
        }))
    } else {
        None
    };

    // Wait for evaluation to finish (the first task in the JoinSet).
    // Then send Stop sentinels to drain all the other queues.
    // Because JoinSet doesn't preserve order, we run the joins to completion
    // and detect failures along the way. Once eval ends, we publish stop
    // sentinels.
    //
    // Simpler approach: wait for the eval child to exit first, then push
    // stops, then drain JoinSet.
    let eval_status = eval_proc.wait().await.context("wait for nix-eval-jobs")?;
    let eval_rc = eval_status.code().unwrap_or(-1);

    // Drain the eval-stderr task now that the child is gone — its pipe
    // has already closed so the drain returns immediately.
    if let Some(h) = eval_stderr_drain {
        let _ = h.await;
    }

    // Push Stop sentinels onto every queue (N per build/upload/etc.; 1 for niks3).
    let build_tx = build_q.sender();
    for _ in 0..max_jobs {
        let _ = build_tx.send(WorkItem::Stop).await;
    }
    drop(build_tx);

    for q in [&upload_q, &cachix_q, &attic_q, &download_q] {
        if let Some(q) = q {
            let tx = q.sender();
            for _ in 0..max_jobs {
                let _ = tx.send(WorkItem::Stop).await;
            }
        }
    }
    if let Some(q) = niks3_q.as_ref() {
        let _ = q.sender().send(WorkItem::Stop).await;
    }

    // Drain workers.
    while let Some(res) = tasks.join_next().await {
        if let Err(e) = res.context("worker task panic")? {
            tracing::warn!("worker error: {e:#}");
        }
    }

    // Stop progress reporter.
    if let Some(h) = progress_handle {
        h.abort();
    }

    // Stop nom + cachix daemon.
    if let Some(mut c) = nom_child.take() {
        ensure_stop(&mut c, "nix-output-monitor").await;
    }
    if let Some(d) = cachix_daemon {
        d.stop().await;
    }

    // Build the final exit code.
    let outcomes = Arc::try_unwrap(outcomes)
        .map_err(|_| anyhow::anyhow!("outcomes still held"))?
        .into_inner();
    let mut rc: u8 = 0;
    for o in &outcomes {
        if !o.success {
            rc = 1;
        }
    }
    if eval_rc != 0 {
        tracing::error!("nix-eval-jobs exited with {eval_rc}");
        rc = 1;
    }

    // Result file output.
    if let Some(path) = &opts.result_file {
        let f =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        match opts.result_format {
            ResultFormat::Json => dump_json(f, &outcomes)?,
            ResultFormat::Junit => dump_junit_xml(f, &opts.display_name(), &outcomes)?,
        }
    }

    // CI step-summary.
    if let Some(path) = ci_summary::ci_summary_file() {
        if let Err(e) = ci_summary::write_ci_summary(&path, &outcomes, rc) {
            tracing::warn!("Failed to write CI summary to {}: {e}", path.display());
        } else {
            tracing::info!("CI summary written to {}", path.display());
        }
    }

    Ok(rc)
}

// ---- Workers -------------------------------------------------------------

async fn run_evaluation(
    eval_stdout: tokio::process::ChildStdout,
    build_tx: Sender<WorkItem<Job>>,
    upload_tx: Option<Sender<WorkItem<Build>>>,
    outcomes: Outcomes,
    opts: &Options,
) -> Result<()> {
    let mut reader = BufReader::new(eval_stdout).lines();
    while let Some(line) = reader.next_line().await? {
        tracing::debug!("{line}");
        let raw: RawEvalLine = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                bail!("Failed to parse line of nix-eval-jobs output: {line} ({e})");
            }
        };
        let attr = raw
            .attr
            .clone()
            .unwrap_or_else(|| "unknown-attribute".into());
        outcomes.lock().await.push(Outcome {
            kind: ResultKind::Eval,
            attr: attr.clone(),
            success: raw.error.is_none(),
            duration: 0.0,
            error: raw.error.clone(),
            log_output: None,
            outputs: None,
        });
        if raw.error.is_some() {
            continue;
        }

        // Cache-status filtering: skip remotely cached; push locally cached
        // jobs to the upload queue without rebuilding.
        let cache_status = raw.cache_status.as_deref();
        match cache_status {
            Some("cached") => continue,
            Some("local") => {
                if let Some(tx) = upload_tx.as_ref() {
                    let drv = raw.drv_path.clone().unwrap_or_default();
                    let outs = raw.outputs.clone();
                    let _ = tx
                        .send(WorkItem::Item(Build {
                            attr: attr.clone(),
                            drv_path: drv,
                            outputs: outs,
                        }))
                        .await;
                }
            }
            Some(_) => {}
            None => {
                if raw.is_cached {
                    continue;
                }
            }
        }

        if let Some(sys) = &raw.system {
            if !opts.systems.is_empty() && !opts.systems.contains(sys) {
                continue;
            }
        }
        let drv_path = raw
            .drv_path
            .as_ref()
            .with_context(|| format!("nix-eval-jobs did not return a drvPath: {line}"))?
            .clone();
        let job = Job {
            attr: attr.clone(),
            drv_path,
            outputs: raw.outputs.clone(),
            system: raw.system.clone(),
        };
        let _ = build_tx.send(WorkItem::Item(job)).await;
    }
    Ok(())
}

async fn run_build_worker(
    _idx: usize,
    build_q: Arc<WorkQueue<WorkItem<Job>>>,
    next_queues: Vec<Sender<WorkItem<Build>>>,
    nom_pipe: Option<Arc<NomPipe>>,
    otel: Option<Arc<PerBuildIngest>>,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(guard) = build_q.pop().await {
        let item = guard.into_inner();
        let job = match item {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(j) => j,
        };
        if !seen.insert(job.drv_path.clone()) {
            continue;
        }
        let build = Build::from_job(job);
        tracing::info!("  building {}", build.attr);
        let start = Instant::now();
        let installable = if opts.eval_mode == EvalMode::Flake {
            if opts.flake_fragment.is_empty() {
                format!("{}#{}", opts.flake_url, build.attr)
            } else {
                format!("{}#{}.{}", opts.flake_url, opts.flake_fragment, build.attr)
            }
        } else {
            build.attr.clone()
        };
        let res = run_nix_build(
            &build,
            &installable,
            &opts,
            nom_pipe.as_deref(),
            otel.as_deref(),
        )
        .await;
        let duration = start.elapsed().as_secs_f64();

        let (success, error, log_output) = match res {
            Ok(br) if br.return_code == 0 => (true, None, None),
            Ok(br) => (
                false,
                Some(format!("build exited with {}", br.return_code)),
                Some(br.log_output),
            ),
            Err(e) => (false, Some(format!("{e:#}")), None),
        };
        let outputs = if !build.outputs.is_empty() {
            Some(
                build
                    .outputs
                    .clone()
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
            )
        } else {
            None
        };
        outcomes.lock().await.push(Outcome {
            kind: ResultKind::Build,
            attr: build.attr.clone(),
            success,
            duration,
            error,
            log_output,
            outputs,
        });
        if !success {
            continue;
        }
        for tx in &next_queues {
            let _ = tx.send(WorkItem::Item(build.clone())).await;
        }
    }
    Ok(())
}

async fn run_upload_worker(
    q: Arc<WorkQueue<WorkItem<Build>>>,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    while let Some(guard) = q.pop().await {
        let item = guard.into_inner();
        let build = match item {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(b) => b,
        };
        let start = Instant::now();
        let rc = upload::nix_copy::upload(&build, &opts).await.unwrap_or(-1);
        push_simple_outcome(
            &outcomes,
            ResultKind::Upload,
            &build.attr,
            rc,
            start,
            "upload",
        )
        .await;
    }
    Ok(())
}

async fn run_cachix_worker(
    q: Arc<WorkQueue<WorkItem<Build>>>,
    socket: PathBuf,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    while let Some(guard) = q.pop().await {
        let item = guard.into_inner();
        let build = match item {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(b) => b,
        };
        let start = Instant::now();
        let rc = upload::cachix::push(&build, &socket, &opts)
            .await
            .unwrap_or(-1);
        push_simple_outcome(
            &outcomes,
            ResultKind::Cachix,
            &build.attr,
            rc,
            start,
            "cachix upload",
        )
        .await;
    }
    Ok(())
}

async fn run_attic_worker(
    q: Arc<WorkQueue<WorkItem<Build>>>,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    while let Some(guard) = q.pop().await {
        let item = guard.into_inner();
        let build = match item {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(b) => b,
        };
        let start = Instant::now();
        let rc = upload::attic::push(&build, &opts).await.unwrap_or(-1);
        push_simple_outcome(
            &outcomes,
            ResultKind::Attic,
            &build.attr,
            rc,
            start,
            "attic upload",
        )
        .await;
    }
    Ok(())
}

async fn run_download_worker(
    q: Arc<WorkQueue<WorkItem<Build>>>,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    while let Some(guard) = q.pop().await {
        let item = guard.into_inner();
        let build = match item {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(b) => b,
        };
        let start = Instant::now();
        let rc = upload::download::download(&build, &opts)
            .await
            .unwrap_or(-1);
        push_simple_outcome(
            &outcomes,
            ResultKind::Download,
            &build.attr,
            rc,
            start,
            "download",
        )
        .await;
    }
    Ok(())
}

async fn run_niks3_worker(
    q: Arc<WorkQueue<WorkItem<Build>>>,
    outcomes: Outcomes,
    opts: Arc<Options>,
) -> Result<()> {
    loop {
        // Wait for at least one item.
        let Some(first_guard) = q.pop().await else {
            return Ok(());
        };
        let first = first_guard.into_inner();
        let first = match first {
            WorkItem::Stop => return Ok(()),
            WorkItem::Item(b) => b,
        };
        // Drain whatever else is queued right now (non-blocking).
        let mut batch: Vec<Build> = vec![first];
        let mut saw_stop = false;
        while q.queued_len() > 0 {
            let Some(g) = q.pop().await else { break };
            match g.into_inner() {
                WorkItem::Item(b) => batch.push(b),
                WorkItem::Stop => {
                    // Re-queue the stop so the next loop iteration sees it
                    // and exits cleanly after the current batch.
                    saw_stop = true;
                    break;
                }
            }
        }
        let start = Instant::now();
        let refs: Vec<&Build> = batch.iter().collect();
        let rc = upload::niks3::push(&refs, &opts).await.unwrap_or(-1);
        let duration = start.elapsed().as_secs_f64() / batch.len().max(1) as f64;
        let mut lock = outcomes.lock().await;
        for b in &batch {
            lock.push(Outcome {
                kind: ResultKind::Niks3,
                attr: b.attr.clone(),
                success: rc == 0,
                duration,
                error: if rc == 0 {
                    None
                } else {
                    Some(format!("niks3 upload exited with {rc}"))
                },
                log_output: None,
                outputs: None,
            });
        }
        if saw_stop {
            return Ok(());
        }
    }
}

async fn push_simple_outcome(
    outcomes: &Outcomes,
    kind: ResultKind,
    attr: &str,
    rc: i32,
    start: Instant,
    op: &str,
) {
    outcomes.lock().await.push(Outcome {
        kind,
        attr: attr.into(),
        success: rc == 0,
        duration: start.elapsed().as_secs_f64(),
        error: if rc == 0 {
            None
        } else {
            Some(format!("{op} exited with {rc}"))
        },
        log_output: None,
        outputs: None,
    });
}

async fn report_progress(
    build_q: Arc<WorkQueue<WorkItem<Job>>>,
    upload_q: Option<Arc<WorkQueue<WorkItem<Build>>>>,
    download_q: Option<Arc<WorkQueue<WorkItem<Build>>>>,
) {
    let mut last = String::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        interval.tick().await;
        let builds = build_q.queued_len() + build_q.running_len();
        let mut now = format!("builds: {builds}");
        if let Some(q) = upload_q.as_ref() {
            now.push_str(&format!(", uploads: {}", q.queued_len() + q.running_len()));
        }
        if let Some(q) = download_q.as_ref() {
            now.push_str(&format!(
                ", downloads: {}",
                q.queued_len() + q.running_len()
            ));
        }
        if now != last {
            tracing::info!("{now}");
            last = now;
        }
    }
}

/// Drain `nix-eval-jobs` stderr line-by-line, mirroring it to the optional
/// stderr_log file and pushing each line into the optional ring sink.
/// Lines matching `is_noisy_eval_line` (host-nix-conf warnings, sqlite
/// eval-cache contention) are dropped at this boundary — they still don't
/// land in the log file. Mirrors the build-stage tee in `nix_build.rs`.
async fn drain_eval_stderr(
    stderr: tokio::process::ChildStderr,
    stderr_log: Option<PathBuf>,
    ring: Option<Arc<LineRing>>,
) {
    use tokio::io::AsyncWriteExt;

    let mut log_writer: Option<tokio::fs::File> = match stderr_log {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|e| {
                    tracing::warn!("open eval stderr_log {}: {e}", path.display());
                    e
                })
                .ok()
        }
        None => None,
    };

    let mut ring_fmt = ring.as_ref().map(|_| RingFormatter::new());
    let mut reader = BufReader::new(stderr).lines();
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if is_noisy_eval_line(&line) {
                    continue;
                }
                if let Some(w) = log_writer.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                if let (Some(ring), Some(fmt)) = (ring.as_ref(), ring_fmt.as_mut()) {
                    if let Some(pretty) = fmt.humanize(line) {
                        ring.push(pretty);
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("eval stderr read: {e}");
                break;
            }
        }
    }

    if let Some(w) = log_writer.as_mut() {
        let _ = w.flush().await;
    }
}
