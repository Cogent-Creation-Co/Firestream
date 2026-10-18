//! Patterns #12, #13, #14 from the plan: typed `FastBuild` builder over
//! `firestream-nix-build` lib (events → spans), span reconcile against
//! `nix-fast-build` JSON result-file, JSON-after-success failure synthesis
//! when the upstream CLI exits before writing results.
//!
//! ## Design
//!
//! Thin typed builder around [`firestream_nix_build::Options`]. The expensive
//! bits — eval, queue management, retries — live in the leaf crate; we only
//! provide a fluent API + a structured result. The reconcile path calls
//! [`otel_cli::reconcile_spans`] directly (no subprocess).
//!
//! The `synth_failure` constructor mirrors the JSON-after-success pattern
//! from `bin/ci/ci-linux.sh:243,488`: when nix-fast-build exits before
//! writing its results JSON we still need a structured failure record per
//! attribute so the reconcile step can mark them red.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use otel_cli::{ReconcileError, ReconcileReport};
use firestream_nix_build::options::{EvalMode, Options, ResultFormat};
use firestream_nix_build::ring::LineRing;
use thiserror::Error;

// Re-export so firestream-ci callers don't need to depend on firestream-nix-build directly.
pub use firestream_nix_build::ring::LineRing as RingSink;

#[derive(Debug, Error)]
pub enum Error {
    #[error("nix: configuration: {0}")]
    Config(String),

    #[error("nix: run failed: {0}")]
    Run(String),

    #[error("nix: reconcile: {0}")]
    Reconcile(#[from] ReconcileError),
}

/// One entry from a nix-fast-build `--result-file` JSON. Used for both
/// real-build results and synthesized failure records.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResultEntry {
    pub attr: String,
    pub success: bool,
    #[serde(rename = "type")]
    pub kind: String,
    pub duration: f64,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<std::collections::BTreeMap<String, String>>,
}

/// Aggregate result of one `FastBuild::run`.
#[derive(Debug, Clone, Default)]
pub struct FastBuildResult {
    /// Return code from the underlying run.
    pub exit_code: u8,
    /// Path to the result-file JSON that the run wrote (when configured).
    pub result_file: Option<PathBuf>,
    /// Path to the spans directory the run wrote into (when configured).
    pub spans_dir: Option<PathBuf>,
}

impl FastBuildResult {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Typed builder for one `nix-fast-build` invocation.
pub struct FastBuild {
    flake_url: String,
    flake_fragment: String,
    eval_mode: EvalMode,
    max_jobs: usize,
    cores: usize,
    keep_going: bool,
    skip_cached: bool,
    no_link: bool,
    nom: bool,
    download: bool,
    result_file: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    ring_sink: Option<Arc<LineRing>>,
    spans_dir: Option<PathBuf>,
    systems: BTreeSet<String>,
    extra_options: Vec<String>,
    limits: Option<crate::limits::Limits>,
    select_expr: Option<String>,
}

impl FastBuild {
    pub fn builder() -> FastBuildBuilder {
        FastBuildBuilder::default()
    }

    /// Execute the configured run via `firestream_nix_build::run::run`.
    ///
    /// Span-dir precedence on the returned [`FastBuildResult`]:
    ///   1. `self.spans_dir` when the builder was given an explicit path.
    ///   2. `OTEL_SPAN_DIR` env when (1) is `None`. This is the bash-bridge
    ///      fallback for callers constructing `FastBuild` without a path.
    pub async fn run(self) -> Result<FastBuildResult, Error> {
        let resolved_spans = resolve_spans_dir(self.spans_dir.as_deref());
        // Resolve the cgroup scope prefix BEFORE `into_options` — the probe is
        // async (it shells out to `systemd-run --version` and
        // `systemctl --user is-active`) and memoised, so this costs one IPC
        // per process regardless of how many attrs are built.
        let scope_prefix = match &self.limits {
            Some(l) => crate::limits::systemd_scope_prefix(l).await,
            None => None,
        };
        let opts = self.into_options(scope_prefix)?;
        let result_file = opts.result_file.clone();

        let rc = firestream_nix_build::run::run(opts)
            .await
            .map_err(|e| Error::Run(format!("{e:#}")))?;

        Ok(FastBuildResult {
            exit_code: rc,
            result_file,
            spans_dir: resolved_spans,
        })
    }

    fn into_options(self, scope_prefix: Option<Vec<String>>) -> Result<Options, Error> {
        if self.flake_url.is_empty() {
            return Err(Error::Config("FastBuild: flake_url is required".into()));
        }
        // `keep_going` is a hint we pass through as a `-k` --option, since
        // upstream Options doesn't model it as a struct field.
        let mut options = self.extra_options;
        if self.keep_going {
            options.push("--option".into());
            options.push("keep-going".into());
            options.push("true".into());
        }
        if self.cores > 0 {
            options.push("--option".into());
            options.push("cores".into());
            options.push(self.cores.to_string());
        }
        // Only the BUILD children get the cgroup scope. `nix log` / `nix copy`
        // / `nix flake metadata` keep the bare `nix_bin`: they are short,
        // memory-trivial, and one transient systemd scope per failure log
        // would be pure noise in the journal.
        let nix_build_bin = match scope_prefix {
            Some(mut p) => {
                p.push("nix".into());
                p.push("build".into());
                p
            }
            None => vec!["nix".into(), "build".into()],
        };
        Ok(Options {
            nix_bin: vec!["nix".into()],
            nix_eval_jobs_bin: vec!["nix-eval-jobs".into()],
            nix_build_bin,
            eval_mode: self.eval_mode,
            flake_url: self.flake_url,
            flake_fragment: self.flake_fragment,
            expr_file: String::new(),
            expr_attr: String::new(),
            expr_args: Vec::new(),
            impure: false,
            options,
            remote: None,
            remote_ssh_options: Vec::new(),
            always_upload_source: false,
            systems: self.systems,
            eval_max_memory_size: 4096,
            skip_cached: self.skip_cached,
            eval_workers: 1,
            max_jobs: self.max_jobs,
            retries: 0,
            debug: false,
            copy_to: None,
            nom: self.nom,
            download: self.download,
            no_link: self.no_link,
            out_link: "result".into(),
            result_format: ResultFormat::Json,
            result_file: self.result_file,
            stderr_log: self.stderr_log,
            ring_sink: self.ring_sink,
            override_inputs: Vec::new(),
            select_expr: self.select_expr,
            reference_lock_file: None,
            cachix_cache: None,
            attic_cache: None,
            attic_ignore_upstream_cache_filter: false,
            attic_push_build_closure: false,
            niks3_server: None,
            otel_ingest: self.spans_dir.is_some(),
            otel_parent_trace: None,
            otel_service: "firestream-ci".into(),
            otel_cli_bin: "otel-cli".into(),
        })
    }
}

/// Builder for [`FastBuild`].
#[derive(Default)]
pub struct FastBuildBuilder {
    flake_url: Option<String>,
    flake_fragment: Option<String>,
    max_jobs: Option<usize>,
    cores: Option<usize>,
    keep_going: bool,
    skip_cached: bool,
    no_link: bool,
    nom: bool,
    download: bool,
    result_file: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    ring_sink: Option<Arc<LineRing>>,
    spans_dir: Option<PathBuf>,
    systems: BTreeSet<String>,
    extra_options: Vec<String>,
    limits: Option<crate::limits::Limits>,
    select_expr: Option<String>,
}

impl FastBuildBuilder {
    /// Run every `nix build` child inside a resource-limited cgroup scope
    /// (`systemd-run --user --scope -p MemoryMax=… -p IOWriteBandwidthMax=…`).
    ///
    /// Silently falls through to unwrapped execution — with a `tracing::warn!`
    /// — when systemd is unavailable, matching [`crate::limits::LimitedCommand`].
    /// Only the build children are wrapped; evaluation and the auxiliary
    /// `nix log` / `nix copy` calls are not.
    pub fn limits(mut self, l: crate::limits::Limits) -> Self {
        self.limits = Some(l);
        self
    }

    /// A `nix-eval-jobs --select` expression (`root: …`) narrowing the
    /// attribute set named by [`FastBuildBuilder::attr`].
    ///
    /// This is what makes ONE invocation over `packages.<system>` build
    /// exactly the profile's declared attrs rather than every package in the
    /// flake — a single evaluation, one bounded job queue, per-attr verdicts
    /// preserved in the result file.
    pub fn select_expr(mut self, expr: impl Into<String>) -> Self {
        self.select_expr = Some(expr.into());
        self
    }
    pub fn flake(mut self, url: impl Into<String>) -> Self {
        self.flake_url = Some(url.into());
        self
    }
    pub fn attr(mut self, fragment: impl Into<String>) -> Self {
        self.flake_fragment = Some(fragment.into());
        self
    }
    pub fn max_jobs(mut self, n: usize) -> Self {
        self.max_jobs = Some(n);
        self
    }
    pub fn cores(mut self, n: usize) -> Self {
        self.cores = Some(n);
        self
    }
    pub fn keep_going(mut self, on: bool) -> Self {
        self.keep_going = on;
        self
    }
    pub fn skip_cached(mut self, on: bool) -> Self {
        self.skip_cached = on;
        self
    }
    pub fn no_link(mut self, on: bool) -> Self {
        self.no_link = on;
        self
    }
    pub fn nom(mut self, on: bool) -> Self {
        self.nom = on;
        self
    }
    pub fn download(mut self, on: bool) -> Self {
        self.download = on;
        self
    }
    pub fn result_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.result_file = Some(path.into());
        self
    }
    /// Tee nix-fast-build's child stderr to this file. Lets the caller
    /// recover the actual failure cause when nix-fast-build exits before
    /// writing its JSON result file.
    pub fn stderr_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.stderr_log = Some(path.into());
        self
    }
    /// Tee each stderr line into a shared in-memory ring buffer for live
    /// tail-N display. The ring receives both eval-stage and build-stage
    /// lines (eval stderr is captured when this is set; otherwise it stays
    /// `Stdio::inherit()` as before).
    pub fn ring_sink(mut self, ring: Arc<LineRing>) -> Self {
        self.ring_sink = Some(ring);
        self
    }
    /// Path where the underlying nix run will deposit its OTel span tree
    /// (`<dir>/<traceHex>/<spanHex>/span.json`). When set, this takes
    /// precedence over the `OTEL_SPAN_DIR` env var on the returned
    /// [`FastBuildResult::spans_dir`] (env is fallback only). The Phase-2
    /// rundir-as-source-of-truth refactor relies on this precedence so the
    /// per-run spans dir from `RunDir::spans_dir()` always wins over a stale
    /// ambient env.
    pub fn spans_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.spans_dir = Some(path.into());
        self
    }
    pub fn system(mut self, sys: impl Into<String>) -> Self {
        self.systems.insert(sys.into());
        self
    }
    pub fn extra_option(mut self, opt: impl Into<String>) -> Self {
        self.extra_options.push(opt.into());
        self
    }
    pub fn build(self) -> Result<FastBuild, Error> {
        let flake_url = self
            .flake_url
            .ok_or_else(|| Error::Config("FastBuild::flake is required".into()))?;
        Ok(FastBuild {
            flake_url,
            flake_fragment: self.flake_fragment.unwrap_or_default(),
            eval_mode: EvalMode::Flake,
            max_jobs: self.max_jobs.unwrap_or(1),
            cores: self.cores.unwrap_or(0),
            keep_going: self.keep_going,
            skip_cached: self.skip_cached,
            no_link: self.no_link,
            nom: self.nom,
            download: self.download,
            result_file: self.result_file,
            stderr_log: self.stderr_log,
            ring_sink: self.ring_sink,
            spans_dir: self.spans_dir,
            systems: self.systems,
            extra_options: self.extra_options,
            limits: self.limits,
            select_expr: self.select_expr,
        })
    }
}

/// Build a `nix-eval-jobs --select` expression that keeps exactly `leaves`
/// from the attrset the fragment names.
///
/// `builtins.listToAttrs` rather than `inherit`, because Nix attribute names
/// like `firestream-log-tests` and `airflow-chart` are not identifiers.
/// `builtins.intersectAttrs`-style filtering would silently drop a typo; this
/// shape makes a missing attr a loud evaluation error naming the attribute,
/// which is the diagnosable failure mode.
pub fn select_expr_for_leaves(leaves: &[String]) -> String {
    let list = leaves
        .iter()
        .map(|l| format!("{:?}", l))
        .collect::<Vec<_>>()
        .join(" ");
    format!("root: builtins.listToAttrs (map (n: {{ name = n; value = root.${{n}}; }}) [ {list} ])")
}

/// In-process span reconcile against a nix-fast-build result-file. Wraps
/// [`otel_cli::reconcile_spans`] so callers don't depend on the otel-cli
/// surface directly.
pub async fn reconcile(result_file: &Path, spans_dir: &Path) -> Result<ReconcileReport, Error> {
    otel_cli::reconcile_spans(result_file, spans_dir, spans_dir)
        .await
        .map_err(Error::from)
}

/// Synthesize a failure entry for an attribute that nix-fast-build didn't
/// have a chance to report on (process exited before writing results JSON,
/// or the attribute was filtered out by eval). Used by callers that need to
/// surface a Required failure even when the run never produced a record.
///
/// See `bin/ci/ci-linux.sh:243,488` for the bash equivalent of this pattern.
pub fn synth_failure(attr: impl Into<String>, reason: impl Into<String>) -> ResultEntry {
    ResultEntry {
        attr: attr.into(),
        success: false,
        kind: "BUILD".to_string(),
        duration: 0.0,
        error: Some(reason.into()),
        outputs: None,
    }
}

/// Append synthetic failure entries to a result-file JSON. If the file does
/// not yet exist (the upstream run died before writing it), a fresh file is
/// created. Idempotent: existing entries with the same attr are not
/// duplicated — the existing entry wins.
pub fn append_synth_failures(result_file: &Path, synth: &[ResultEntry]) -> Result<(), Error> {
    use serde_json::{Map, Value};

    let mut doc: Value = if result_file.exists() {
        let bytes = std::fs::read(result_file)
            .map_err(|e| Error::Run(format!("read {}: {e}", result_file.display())))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Run(format!("parse {}: {e}", result_file.display())))?
    } else {
        let mut m = Map::new();
        m.insert("results".to_string(), Value::Array(Vec::new()));
        Value::Object(m)
    };

    let arr = doc
        .as_object_mut()
        .and_then(|o| o.get_mut("results"))
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| Error::Run("results array missing".to_string()))?;

    let existing: BTreeSet<String> = arr
        .iter()
        .filter_map(|e| e.get("attr").and_then(|a| a.as_str()).map(String::from))
        .collect();

    for entry in synth {
        if existing.contains(&entry.attr) {
            continue;
        }
        let v =
            serde_json::to_value(entry).map_err(|e| Error::Run(format!("serialize synth: {e}")))?;
        arr.push(v);
    }

    let body =
        serde_json::to_vec_pretty(&doc).map_err(|e| Error::Run(format!("serialize doc: {e}")))?;
    std::fs::write(result_file, body)
        .map_err(|e| Error::Run(format!("write {}: {e}", result_file.display())))?;
    Ok(())
}

/// Spans-dir resolution for [`FastBuild::run`]. Builder field wins; env
/// fallback is consulted only when the builder didn't supply a path. Pulled
/// out of `run()` as a free function so the precedence is unit-testable
/// without a live tokio runtime and without touching `OTEL_SPAN_DIR`
/// process-globally during tests.
fn resolve_spans_dir(builder: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = builder {
        return Some(p.to_path_buf());
    }
    std::env::var_os("OTEL_SPAN_DIR").map(PathBuf::from)
}

/// Roots-based Nix store GC. Mirrors `bin/ci/ci-gc.sh`: enumerate every
/// `checks.<system>.*` and `packages.<system>.*` attribute, register each as
/// an indirect GC root under `roots_dir`, then run `nix-collect-garbage`.
///
/// This realises a "post-source-load" prune of the warm /nix store inside the
/// builder container. Per-root failures are advisory — a broken derivation
/// must NOT skip the trailing collection step; otherwise GC silently never
/// runs (see `ci-gc.sh:74-94`).
///
/// `phase0_max_jobs` / `phase0_cores` map onto `nix-store --option max-jobs
/// --option cores`; the bash defaults (1, 2) keep cold-cache lance-core
/// rustc bursts inside the 32G container ceiling.
pub fn gc(
    flake_path: &Path,
    nix_system: &str,
    roots_dir: &Path,
    phase0_max_jobs: u32,
    phase0_cores: u32,
    stderr_log: Option<&Path>,
    ring_sink: Option<Arc<LineRing>>,
) -> Result<GcReport, Error> {
    use std::process::Command as StdCommand;

    if !flake_path.exists() {
        return Err(Error::Config(format!(
            "gc: flake path does not exist: {}",
            flake_path.display()
        )));
    }

    // Reset the roots dir so prior runs' GC roots don't pin stale outputs.
    if roots_dir.exists() {
        std::fs::remove_dir_all(roots_dir)
            .map_err(|e| Error::Run(format!("rm -rf {}: {e}", roots_dir.display())))?;
    }
    std::fs::create_dir_all(roots_dir)
        .map_err(|e| Error::Run(format!("mkdir -p {}: {e}", roots_dir.display())))?;

    let routing = StderrRouting {
        log: stderr_log.map(PathBuf::from),
        ring: ring_sink,
    };

    // Enumerate roots via `nix flake show --json`. `--quiet` suppresses
    // the eval-walk "evaluating X" chatter (otherwise the tidy stderr log
    // grows to ~3.6 MB on a full flake walk).
    let mut show_cmd = StdCommand::new("nix");
    show_cmd
        .arg("--quiet")
        .arg("flake")
        .arg("show")
        .arg("--json")
        .arg("--no-update-lock-file")
        .arg(flake_path);
    let show_out = run_with_stderr_capture(&mut show_cmd, &routing)
        .map_err(|e| Error::Run(format!("nix flake show: {e}")))?;
    if !show_out.status.success() {
        return Err(Error::Run(format!(
            "nix flake show failed (exit {})",
            show_out.status,
        )));
    }
    let show: serde_json::Value = serde_json::from_slice(&show_out.stdout)
        .map_err(|e| Error::Run(format!("parse nix flake show output: {e}")))?;

    let mut roots: Vec<String> = Vec::new();
    for kind in &["checks", "packages"] {
        let Some(map) = show
            .get(*kind)
            .and_then(|v| v.get(nix_system))
            .and_then(|v| v.as_object())
        else {
            continue;
        };
        for attr in map.keys() {
            roots.push(format!(".#{kind}.{nix_system}.{attr}"));
        }
    }

    let mut roots_failed = 0u32;
    let mut deleted_paths: Vec<String> = Vec::new();

    // Phase A: resolve each flake fragment to its .drv path *in parallel*.
    // The previous implementation walked roots serially, paying one full nix
    // eval invocation per attribute. With a monorepo flake that dominated
    // tidy wall-clock. `std::thread::scope` keeps borrow-safety with the
    // captured `routing` / `flake_path` references and avoids an async
    // runtime change to this sync entry point. Sanitise the symlink name
    // here too — mirrors the bash `echo "$r" | tr './:#' '_'`.
    let routing_ref = &routing;
    let resolved: Vec<(String, String, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = roots
            .iter()
            .map(|r| {
                let r_owned = r.clone();
                let name: String = r_owned
                    .chars()
                    .map(|c| match c {
                        '.' | '/' | ':' | '#' => '_',
                        other => other,
                    })
                    .collect();
                s.spawn(move || {
                    let mut drv_cmd = StdCommand::new("nix");
                    drv_cmd
                        .args([
                            "--quiet",
                            "path-info",
                            "--derivation",
                            "--no-update-lock-file",
                            &r_owned,
                        ])
                        .current_dir(flake_path);
                    let drv_out = run_with_stderr_capture(&mut drv_cmd, routing_ref);
                    let drv = match drv_out {
                        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .map(|s| s.trim().to_string()),
                        _ => None,
                    };
                    (r_owned, name, drv)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("scoped resolve thread panicked"))
            .collect()
    });

    // Partition into successfully-resolved roots vs unresolvable ones.
    let mut to_register: Vec<(String, String)> = Vec::new(); // (sanitised name, drv path)
    for (r, name, drv_opt) in resolved {
        match drv_opt {
            Some(drv) => to_register.push((name, drv)),
            None => {
                tracing::warn!(target: "firestream_ci::nix::gc", root = %r, "cannot resolve derivation");
                roots_failed += 1;
            }
        }
    }

    // Phase B: mass-realise all resolved drvs in one nix-store invocation.
    // Single fork, single store-lock acquisition, shared substitution queue.
    // `keep-going true` ensures one bad drv doesn't poison the warm-up; the
    // per-drv `--add-root` step below is the authoritative success signal,
    // so the mass-realise exit code is intentionally ignored.
    if !to_register.is_empty() {
        let mut realise_cmd = StdCommand::new("nix-store");
        realise_cmd
            .arg("--quiet")
            .arg("--option")
            .arg("max-jobs")
            .arg(phase0_max_jobs.to_string())
            .arg("--option")
            .arg("cores")
            .arg(phase0_cores.to_string())
            .arg("--option")
            .arg("keep-going")
            .arg("true")
            .arg("-r");
        for (_, drv) in &to_register {
            realise_cmd.arg(drv);
        }
        realise_cmd.current_dir(flake_path);
        let _ = run_with_stderr_capture(&mut realise_cmd, &routing);
    }

    // Phase C: register each indirect GC root in parallel. Outputs are
    // already in store from Phase B, so each call is a fast metadata
    // operation (symlink + entry under /nix/var/nix/gcroots/auto). Per-drv
    // failure is what flips `roots_failed`, preserving the original
    // `roots_ok + roots_failed == roots_total` invariant.
    let registered: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = to_register
            .iter()
            .map(|(name, drv)| {
                let symlink = roots_dir.join(name);
                let drv_owned = drv.clone();
                s.spawn(move || {
                    let mut add_root_cmd = StdCommand::new("nix-store");
                    add_root_cmd
                        .arg("--quiet")
                        .arg("--option")
                        .arg("max-jobs")
                        .arg(phase0_max_jobs.to_string())
                        .arg("--option")
                        .arg("cores")
                        .arg(phase0_cores.to_string())
                        .arg("--add-root")
                        .arg(&symlink)
                        .arg("--indirect")
                        .arg("-r")
                        .arg(&drv_owned)
                        .current_dir(flake_path);
                    let out = run_with_stderr_capture(&mut add_root_cmd, routing_ref);
                    matches!(out, Ok(o) if o.status.success())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("scoped register thread panicked"))
            .collect()
    });

    let mut roots_ok = 0u32;
    for (i, ok) in registered.iter().enumerate() {
        if *ok {
            roots_ok += 1;
        } else {
            tracing::warn!(
                target: "firestream_ci::nix::gc",
                root = %to_register[i].0,
                "failed to register root"
            );
            roots_failed += 1;
        }
    }

    // Trailing `nix-collect-garbage`. Captures stdout for deleted-path counting.
    // `--quiet` keeps stderr lean — `nix-collect-garbage` is otherwise chatty
    // ("evaluating X") and the tidy stderr log balloons accordingly.
    //
    // CAUTION: `--delete-older-than 7d` does NOT spare store paths by age. It
    // only deletes *profile generations* older than 7d; the store sweep that
    // then runs deletes every path not reachable from a GC root, however
    // recently it was built. Cache warmth therefore depends entirely on roots
    // existing — recent-but-unrooted outputs are reaped here. This `gc()` path
    // registers indirect roots for all flake checks/packages above before
    // collecting, so they survive; the collect-only tidy path
    // (`collect_garbage`) relies on per-build roots from `register_indirect_roots`.
    let mut gc_cmd = StdCommand::new("nix-collect-garbage");
    gc_cmd.arg("--quiet").arg("--delete-older-than").arg("7d");
    let gc_out = run_with_stderr_capture(&mut gc_cmd, &routing)
        .map_err(|e| Error::Run(format!("nix-collect-garbage: {e}")))?;
    if !gc_out.status.success() {
        return Err(Error::Run(format!(
            "nix-collect-garbage failed (exit {})",
            gc_out.status,
        )));
    }
    // `nix-collect-garbage` prints "deleting '/nix/store/…'" lines.
    for line in String::from_utf8_lossy(&gc_out.stdout).lines() {
        if let Some(rest) = line.strip_prefix("deleting '") {
            if let Some(path) = rest.strip_suffix("'") {
                deleted_paths.push(path.to_string());
            }
        }
    }

    Ok(GcReport {
        roots_ok,
        roots_failed,
        roots_total: roots.len() as u32,
        deleted_paths,
    })
}

/// Collect-only Nix store GC. Runs `nix-collect-garbage --quiet
/// --delete-older-than <spec>` and nothing else — no flake enumeration, no
/// per-attr `nix-store -r` warm-up, no root registration. Intended for the
/// in-CI tidy phase, where roots are created at build time by
/// [`register_indirect_roots`] instead of by a separate enumeration pass.
///
/// Compared to [`gc`], this returns in seconds when nothing is eligible for
/// deletion. Use [`gc`] from the standalone `firestream-ci nix gc` CLI when a
/// belt-and-suspenders pre-root pass is genuinely wanted.
pub fn collect_garbage(
    delete_older_than: &str,
    stderr_log: Option<&Path>,
    ring_sink: Option<Arc<LineRing>>,
) -> Result<CollectReport, Error> {
    use std::process::Command as StdCommand;

    let routing = StderrRouting {
        log: stderr_log.map(PathBuf::from),
        ring: ring_sink,
    };

    let mut gc_cmd = StdCommand::new("nix-collect-garbage");
    gc_cmd
        .arg("--quiet")
        .arg("--delete-older-than")
        .arg(delete_older_than);
    let gc_out = run_with_stderr_capture(&mut gc_cmd, &routing)
        .map_err(|e| Error::Run(format!("nix-collect-garbage: {e}")))?;
    if !gc_out.status.success() {
        return Err(Error::Run(format!(
            "nix-collect-garbage failed (exit {})",
            gc_out.status,
        )));
    }
    let mut deleted_paths: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&gc_out.stdout).lines() {
        if let Some(rest) = line.strip_prefix("deleting '") {
            if let Some(path) = rest.strip_suffix("'") {
                deleted_paths.push(path.to_string());
            }
        }
    }
    Ok(CollectReport { deleted_paths })
}

/// Register a batch of indirect GC roots for store paths that are already
/// in the local store. For each `(name, storepath)`, runs
/// `nix-store --add-root <roots_dir>/<name> --indirect <storepath>` in
/// parallel — no `-r` flag, since the path is already present, so each
/// call is a pure metadata + symlink op (microseconds). The roots dir is
/// created if missing; existing symlinks are overwritten by `nix-store`'s
/// own atomic-rename behavior.
///
/// Per-item failures are advisory and counted in [`RegisterReport::failed`];
/// they do not abort the batch. Used by the build phase to root each
/// just-built attr's outputs so they survive [`collect_garbage`].
pub fn register_indirect_roots(roots_dir: &Path, items: &[(String, String)]) -> RegisterReport {
    use std::process::Command as StdCommand;

    if items.is_empty() {
        return RegisterReport { ok: 0, failed: 0 };
    }
    if let Err(e) = std::fs::create_dir_all(roots_dir) {
        tracing::warn!(
            target: "firestream_ci::nix::register",
            dir = %roots_dir.display(),
            error = %e,
            "cannot create roots dir; skipping registration"
        );
        return RegisterReport {
            ok: 0,
            failed: items.len() as u32,
        };
    }

    let routing = StderrRouting {
        log: None,
        ring: None,
    };
    let routing_ref = &routing;

    let outcomes: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = items
            .iter()
            .map(|(name, storepath)| {
                let symlink = roots_dir.join(name);
                let storepath_owned = storepath.clone();
                s.spawn(move || {
                    let mut cmd = StdCommand::new("nix-store");
                    // `--add-root`/`--indirect` decorate the `--realise` (`-r`)
                    // operation; without `-r` modern nix-store reports
                    // "no operation specified" and the root is never created
                    // (silent, since this batch is best-effort). The path is
                    // already in the store, so `-r` is a fast no-op that just
                    // anchors the gcroot. Mirrors the realise+add-root call in
                    // `collect_garbage`'s Phase C.
                    cmd.arg("--quiet")
                        .arg("--add-root")
                        .arg(&symlink)
                        .arg("--indirect")
                        .arg("-r")
                        .arg(&storepath_owned);
                    let out = run_with_stderr_capture(&mut cmd, routing_ref);
                    matches!(out, Ok(o) if o.status.success())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("scoped register thread panicked"))
            .collect()
    });

    let mut ok = 0u32;
    let mut failed = 0u32;
    for (i, success) in outcomes.iter().enumerate() {
        if *success {
            ok += 1;
        } else {
            tracing::warn!(
                target: "firestream_ci::nix::register",
                name = %items[i].0,
                storepath = %items[i].1,
                "failed to register indirect root"
            );
            failed += 1;
        }
    }
    RegisterReport { ok, failed }
}

/// Where stderr lines from a `gc`-spawned child go. When both fields are
/// `None`, lines are written straight to the calling process's stderr —
/// matching the original inherit-stdio behavior so existing callers don't
/// change. When either is `Some`, the dashboard owns the terminal and the
/// child's raw output must not leak.
struct StderrRouting {
    log: Option<PathBuf>,
    ring: Option<Arc<LineRing>>,
}

impl StderrRouting {
    fn inherit_to_terminal(&self) -> bool {
        self.log.is_none() && self.ring.is_none()
    }
}

/// Captured output of one piped child run.
struct PipedOutput {
    stdout: Vec<u8>,
    status: std::process::ExitStatus,
}

/// Spawn the configured `Command` with both stdout and stderr piped, drain
/// stderr line-by-line through the routing sinks on a worker thread, read
/// stdout into memory, then wait. The terminal only sees stderr when the
/// caller hasn't opted in to capture (`routing.inherit_to_terminal()`).
fn run_with_stderr_capture(
    cmd: &mut std::process::Command,
    routing: &StderrRouting,
) -> std::io::Result<PipedOutput> {
    use std::io::Read;
    use std::process::Stdio;

    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stderr = child
        .stderr
        .take()
        .expect("piped stderr; checked by Stdio::piped above");
    let drain = drain_stderr(
        stderr,
        routing.log.clone(),
        routing.ring.clone(),
        routing.inherit_to_terminal(),
    );
    let mut stdout_buf = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout_buf)?;
    }
    let status = child.wait()?;
    let _ = drain.join();
    Ok(PipedOutput {
        stdout: stdout_buf,
        status,
    })
}

/// Drain a child's stderr in a background thread. Each line is appended to
/// the log file (when set), pushed into the `LineRing` (when set), and —
/// only when neither sink is configured — echoed to this process's stderr.
fn drain_stderr(
    stderr: std::process::ChildStderr,
    log_path: Option<PathBuf>,
    ring: Option<Arc<LineRing>>,
    inherit: bool,
) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead, BufReader, Write};
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut log = log_path.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
        for line in reader.lines().map_while(Result::ok) {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if let Some(r) = ring.as_ref() {
                r.push(line.clone());
            }
            if inherit {
                eprintln!("{line}");
            }
        }
    })
}

/// Summary of a [`gc`] run. `roots_ok + roots_failed == roots_total`.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    /// Roots successfully realised and registered as indirect GC roots.
    pub roots_ok: u32,
    /// Roots that failed to resolve or realise (advisory; the collection
    /// step still ran).
    pub roots_failed: u32,
    /// Total roots discovered via `nix flake show`.
    pub roots_total: u32,
    /// Deleted store paths parsed from `nix-collect-garbage` stdout.
    pub deleted_paths: Vec<String>,
}

/// Summary of a [`collect_garbage`] run.
#[derive(Debug, Clone, Default)]
pub struct CollectReport {
    /// Deleted store paths parsed from `nix-collect-garbage` stdout.
    pub deleted_paths: Vec<String>,
}

/// Summary of a [`register_indirect_roots`] batch. `ok + failed` equals the
/// number of items passed in.
#[derive(Debug, Clone, Default)]
pub struct RegisterReport {
    /// Roots successfully registered.
    pub ok: u32,
    /// Roots whose `nix-store --add-root` invocation failed (advisory).
    pub failed: u32,
}

/// Canonical Nix resource-enforcement settings that every CI nix invocation
/// must run with. Ported from the bash `nix_config_string` (bin/_lib.sh):
///
///   use-cgroups = true  — Nix ≥2.18 places each builder in its own child
///                         cgroup so `memory.peak` is readable per-derivation;
///                         this feeds the OTel `memory.*` span attributes.
///   keep-failed = true  — preserves builder tmp dirs so `nix log <drv>` can
///                         surface the failure tail into `build.log.tail` spans.
///
/// Boolean/additive settings only — deliberately NOT including list-valued
/// keys like `experimental-features`, because `NIX_CONFIG` *replaces* (does
/// not merge) a setting, so clobbering the dev-shell's feature list would be a
/// regression.
///
/// `use-cgroups` is CONDITIONAL, and that is load-bearing rather than cautious.
/// It is gated behind Nix's `cgroups` *experimental feature*; on a daemon
/// without it, setting it is not ignored — nix hard-errors with
/// `experimental Nix feature 'cgroups' is disabled` and **every derivation in
/// the run fails**. The origin repo enabled the feature daemon-wide, so the
/// bash this was ported from could assume it; Firestream cannot. Since we
/// deliberately refuse to write `experimental-features` (see above), the only
/// correct move is to probe and omit. Found by running the pipeline on a stock
/// NixOS host, where it turned all 13 verify checks red in a single stroke.
const CANONICAL_NIX_SETTINGS: &[(&str, &str)] = &[("keep-failed", "true")];

/// Settings that are only safe to apply when the daemon actually supports
/// them. Each entry is `(key, value, probe)`.
const CONDITIONAL_NIX_SETTINGS: &[(&str, &str)] = &[("use-cgroups", "true")];

/// Impure: does this Nix support `use-cgroups`? True only when the `cgroups`
/// experimental feature is enabled, which is what the setting is gated behind.
/// Any probe failure answers "no" — the setting is an observability nicety
/// (per-derivation `memory.peak` for OTel span attributes), so degrading is
/// always preferable to failing the build.
fn supports_use_cgroups() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let out = std::process::Command::new("nix")
        .args(["config", "show", "experimental-features"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .any(|f| f == "cgroups"),
        _ => false,
    }
}

/// Pure: merge the canonical resource settings into an existing `NIX_CONFIG`
/// value (newline-separated `key = value` lines, per the Nix env-var format).
/// Existing keys always win — the operator (or the Makefile's
/// `warn-dirty = false`) is never clobbered.
pub fn merge_nix_config(existing: Option<&str>) -> String {
    merge_nix_config_with(existing, &[])
}

/// Pure core of [`merge_nix_config`]. `extra` carries the conditional settings
/// the caller's probe approved, so the merge itself stays testable without
/// shelling out to `nix`.
pub fn merge_nix_config_with(existing: Option<&str>, extra: &[(&str, &str)]) -> String {
    let existing = existing.unwrap_or("");
    let present: BTreeSet<&str> = existing
        .lines()
        .filter_map(|l| l.split('=').next())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();

    let mut out = existing.trim_end().to_string();
    for (key, val) in CANONICAL_NIX_SETTINGS.iter().chain(extra.iter()) {
        if !present.contains(*key) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(val);
        }
    }
    out
}

/// Impure: read `NIX_CONFIG` from the process env, merge in the canonical
/// resource settings, and write it back so every child nix process — the
/// `FastBuild` subprocesses *and* the direct `nix-store` / `nix-collect-garbage`
/// calls in [`gc`] — inherits them. Mirrors the bash
/// `export NIX_CONFIG="$(nix_config_string)"`. Returns the applied value.
pub fn apply_canonical_nix_config() -> String {
    let extra: &[(&str, &str)] = if supports_use_cgroups() {
        CONDITIONAL_NIX_SETTINGS
    } else {
        tracing::debug!(
            target: "firestream_ci::nix",
            "use-cgroups omitted: nix `cgroups` experimental feature is not enabled. \
             Per-derivation memory.peak span attributes will be absent; builds are \
             otherwise unaffected. Enable with `extra-experimental-features = cgroups` \
             in nix.conf if you want them."
        );
        &[]
    };
    let merged = merge_nix_config_with(std::env::var("NIX_CONFIG").ok().as_deref(), extra);
    std::env::set_var("NIX_CONFIG", &merged);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_flake() {
        let r = FastBuild::builder().build();
        assert!(matches!(r, Err(Error::Config(_))));
    }

    #[test]
    fn builder_minimal_succeeds() {
        let b = FastBuild::builder()
            .flake(".#")
            .attr("checks")
            .max_jobs(4)
            .cores(2)
            .keep_going(true)
            .system("x86_64-linux")
            .build()
            .unwrap();
        assert_eq!(b.flake_url, ".#");
        assert_eq!(b.flake_fragment, "checks");
        assert_eq!(b.max_jobs, 4);
        assert!(b.keep_going);
    }

    #[test]
    fn synth_failure_shape() {
        let e = synth_failure("demo-server", "no result file");
        assert_eq!(e.attr, "demo-server");
        assert!(!e.success);
        assert_eq!(e.kind, "BUILD");
        assert!(e.error.as_deref() == Some("no result file"));
    }

    #[test]
    fn append_synth_failures_creates_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("result.json");
        let entries = vec![synth_failure("a", "boom")];
        append_synth_failures(&path, &entries).unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let arr = doc.get("results").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("attr").unwrap(), "a");
    }

    #[test]
    fn gc_rejects_missing_flake_path() {
        let r = gc(
            std::path::Path::new("/definitely/does/not/exist"),
            "x86_64-linux",
            std::path::Path::new("/tmp/firestream-ci-gc-roots-test"),
            1,
            2,
            None,
            None,
        );
        assert!(matches!(r, Err(Error::Config(_))));
    }

    /// `run_with_stderr_capture` should:
    ///   - leave the parent's stderr alone (so the dashboard isn't clobbered),
    ///   - tee every child stderr line into the `LineRing`,
    ///   - tee every child stderr line into the on-disk log file,
    ///   - return the child's stdout intact for callers that parse it.
    ///
    /// We exercise it with a `sh -c` child that writes a known mix of
    /// stdout/stderr lines. This is a regression test for the Phase 2 fix:
    /// the original `nix::gc` invoked `Command::output()` (which inherits
    /// stderr) and the `--add-root -r` step ran with `.status()` which
    /// inherits everything — both leaked nix's build chatter onto the
    /// superconsole spinner.
    #[test]
    fn stderr_capture_routes_to_ring_and_log_not_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gc.stderr.log");
        let ring = Arc::new(LineRing::with_capacity(6));
        let routing = StderrRouting {
            log: Some(log.clone()),
            ring: Some(ring.clone()),
        };
        assert!(!routing.inherit_to_terminal());

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("echo to-stdout; echo err-1 >&2; echo err-2 >&2; echo err-3 >&2");
        let out = run_with_stderr_capture(&mut cmd, &routing).expect("spawn");
        assert!(out.status.success(), "child exit: {:?}", out.status);
        // stdout came back intact for the caller.
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "to-stdout");

        // Log file got every stderr line, in order, newline-terminated.
        let log_body = std::fs::read_to_string(&log).expect("log written");
        assert_eq!(log_body, "err-1\nerr-2\nerr-3\n");

        // Ring buffer holds the same lines (capacity 6, three pushes).
        let snap = ring.snapshot();
        assert_eq!(snap, vec!["err-1", "err-2", "err-3"]);
    }

    /// With no sinks configured, the helper falls back to inheriting stderr
    /// to the terminal — preserves the standalone `firestream-ci nix gc` CLI's
    /// existing behavior.
    #[test]
    fn stderr_capture_inherit_when_no_sinks() {
        let routing = StderrRouting {
            log: None,
            ring: None,
        };
        assert!(routing.inherit_to_terminal());

        // Just verify the helper runs without panic when both sinks are off.
        // We don't assert on this process's stderr (it would race with the
        // test harness's captured output).
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("echo hi >&2");
        let out = run_with_stderr_capture(&mut cmd, &routing).expect("spawn");
        assert!(out.status.success());
    }

    /// Builder field wins over `OTEL_SPAN_DIR` env. We test the pure helper
    /// (`resolve_spans_dir`) rather than spinning up `FastBuild::run()` —
    /// `env::set_var` is process-global and would race with sibling tests;
    /// the pure helper takes the builder path as an arg so we can drive both
    /// branches deterministically.
    #[test]
    fn run_prefers_builder_spans_dir_over_env() {
        let tmp_a = std::path::PathBuf::from("/tmp/firestream-ci-spans-a");
        // Pretend the ambient env has a different path. We don't actually
        // set OTEL_SPAN_DIR here — the helper signature already isolates the
        // builder branch, so this test cannot race with anything that
        // touches the env. The env-fallback branch is covered by
        // `run_falls_back_to_env_when_builder_unset` below.
        let resolved = resolve_spans_dir(Some(&tmp_a));
        assert_eq!(resolved, Some(tmp_a));
    }

    /// When the builder has no spans-dir path, fall back to the env var
    /// (bash-bridge behavior — used by callers that didn't migrate to the
    /// typed builder yet).
    #[test]
    fn run_falls_back_to_env_when_builder_unset() {
        // We can't safely set/unset OTEL_SPAN_DIR here without serializing
        // with the rest of the test suite. Probe the current env state and
        // assert the helper agrees: if env is set, helper returns Some(it);
        // if env is unset, helper returns None.
        let env = std::env::var_os("OTEL_SPAN_DIR").map(PathBuf::from);
        let resolved = resolve_spans_dir(None);
        assert_eq!(resolved, env);
    }

    #[test]
    fn append_synth_failures_skips_duplicate_attr() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("result.json");
        std::fs::write(
            &path,
            br#"{"results":[{"attr":"a","success":true,"type":"BUILD","duration":1.0,"error":null}]}"#,
        )
        .unwrap();
        let entries = vec![synth_failure("a", "should not override")];
        append_synth_failures(&path, &entries).unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let arr = doc.get("results").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("success").unwrap(), true);
    }

    #[test]
    fn merge_nix_config_into_empty_adds_canonical() {
        let merged = merge_nix_config(None);
        assert!(merged.contains("keep-failed = true"));
    }

    #[test]
    fn merge_nix_config_preserves_existing_and_appends() {
        // The Makefile launches firestream-ci with NIX_CONFIG="warn-dirty = false".
        let merged = merge_nix_config(Some("warn-dirty = false"));
        assert!(merged.contains("warn-dirty = false"));
        assert!(merged.contains("keep-failed = true"));
    }

    #[test]
    fn merge_nix_config_does_not_clobber_operator_override() {
        // Operator explicitly disabled keep-failed — we must not flip it back on.
        let merged = merge_nix_config(Some("keep-failed = false"));
        assert!(merged.contains("keep-failed = false"));
        assert!(!merged.contains("keep-failed = true"));
    }

    /// `use-cgroups` must NEVER be emitted unconditionally. On a daemon without
    /// the `cgroups` experimental feature, nix hard-errors and every derivation
    /// in the run fails — which is exactly how this was found.
    #[test]
    fn use_cgroups_is_not_in_the_unconditional_set() {
        assert!(!merge_nix_config(None).contains("use-cgroups"));
        assert!(!merge_nix_config(Some("warn-dirty = false")).contains("use-cgroups"));
    }

    /// ...but it is still applied when the probe approves it, so the
    /// per-derivation `memory.peak` span attributes survive on hosts that
    /// support them.
    #[test]
    fn use_cgroups_is_appended_when_the_probe_approves() {
        let merged = merge_nix_config_with(None, CONDITIONAL_NIX_SETTINGS);
        assert!(merged.contains("use-cgroups = true"));
        assert!(merged.contains("keep-failed = true"));
    }

    /// An operator opt-out still wins over the conditional set.
    #[test]
    fn conditional_settings_do_not_clobber_operator_override() {
        let merged = merge_nix_config_with(Some("use-cgroups = false"), CONDITIONAL_NIX_SETTINGS);
        assert!(merged.contains("use-cgroups = false"));
        assert!(!merged.contains("use-cgroups = true"));
    }
}

#[cfg(test)]
mod select_expr_tests {
    use super::*;

    #[test]
    fn select_expr_quotes_non_identifier_attribute_names() {
        // `firestream-log-tests` and `airflow-chart` are not Nix identifiers,
        // which is exactly why this is `listToAttrs` over quoted strings and
        // not an `inherit`.
        let e = select_expr_for_leaves(&[
            "firestream-log-tests".to_string(),
            "airflow-chart".to_string(),
        ]);
        assert_eq!(
            e,
            "root: builtins.listToAttrs (map (n: { name = n; value = root.${n}; }) \
             [ \"firestream-log-tests\" \"airflow-chart\" ])"
                .replace("             ", "")
        );
    }

    #[test]
    fn empty_leaf_list_yields_an_empty_attrset() {
        let e = select_expr_for_leaves(&[]);
        assert!(e.starts_with("root: builtins.listToAttrs"));
        assert!(e.ends_with("[  ])"));
    }
}
