//! Runtime options for a `firestream-nix-build` invocation. Mirrors the Python
//! `Options` dataclass in `nix_fast_build/__init__.py:68-137`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ring::LineRing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultFormat {
    Json,
    Junit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvalMode {
    Flake,
    Expr,
}

/// Two-tuple override-input: `(input_path, flake_url)`.
pub type OverrideInput = (String, String);

#[derive(Clone, Debug)]
pub struct Options {
    pub nix_bin: Vec<String>,
    pub nix_eval_jobs_bin: Vec<String>,
    pub nix_build_bin: Vec<String>,

    pub eval_mode: EvalMode,
    pub flake_url: String,
    pub flake_fragment: String,
    pub expr_file: String,
    pub expr_attr: String,
    /// Pre-flattened `--arg`/`--argstr`/`--include` for non-flake mode.
    pub expr_args: Vec<String>,
    pub impure: bool,

    /// Pre-flattened `--option NAME VALUE` pairs (already in argv form).
    pub options: Vec<String>,

    pub remote: Option<String>,
    /// Pre-flattened `-o NAME=VALUE` ssh options.
    pub remote_ssh_options: Vec<String>,
    pub always_upload_source: bool,

    pub systems: BTreeSet<String>,
    pub eval_max_memory_size: u64,
    pub skip_cached: bool,
    pub eval_workers: usize,
    pub max_jobs: usize,
    pub retries: u32,
    pub debug: bool,

    pub copy_to: Option<String>,
    pub nom: bool,
    pub download: bool,
    pub no_link: bool,
    pub out_link: String,

    pub result_format: ResultFormat,
    pub result_file: Option<PathBuf>,

    /// When `Some`, tee child `nix`/`nix-build` stderr to this file (append
    /// mode) alongside whatever existing nom/otel sinks are active. Lets a
    /// caller recover the actual failure cause when `nix-fast-build` exits
    /// before writing its JSON result file. Independent of `nom`/otel; if
    /// both are off and `stderr_log` is `None`, stderr is `inherit`ed as
    /// before.
    pub stderr_log: Option<PathBuf>,

    /// When `Some`, push each stderr line into this in-memory ring buffer
    /// alongside the file/nom/otel sinks. Sized for a tail-N preview pane
    /// in a live UI (see `crate::ring::LineRing`). Independent of all other
    /// sinks. Setting this also flips `nix-eval-jobs` stderr from
    /// `Stdio::inherit()` to a captured pipe so the same ring receives both
    /// eval-stage and build-stage lines.
    pub ring_sink: Option<Arc<LineRing>>,

    pub override_inputs: Vec<OverrideInput>,
    pub select_expr: Option<String>,
    pub reference_lock_file: Option<String>,

    pub cachix_cache: Option<String>,

    pub attic_cache: Option<String>,
    pub attic_ignore_upstream_cache_filter: bool,
    pub attic_push_build_closure: bool,

    pub niks3_server: Option<String>,

    // ConceptDB fork: in-process OTel ingest (PRD §9.2). One ingest state
    // machine per build keeps each nix process's activity-id space isolated.
    pub otel_ingest: bool,
    pub otel_parent_trace: Option<String>,
    pub otel_service: String,
    /// Retained for CLI compatibility with the Python fork. Unused: the Rust
    /// port runs the ingest in-process and does not spawn `otel-cli`.
    pub otel_cli_bin: String,
}

impl Options {
    /// Wrap `args` in the standard nix invocation: `<nix_bin> --experimental-features
    /// 'nix-command flakes' --option warn-dirty false <args…>`.
    ///
    /// The dirty-tree warning is suppressed unconditionally: CI runs against
    /// the user's worktree by design and the warning prints once per parallel
    /// task — pure noise.
    pub fn nix_command(&self, args: &[&str]) -> Vec<String> {
        let mut out = Vec::with_capacity(self.nix_bin.len() + 5 + args.len());
        out.extend(self.nix_bin.iter().cloned());
        out.push("--experimental-features".into());
        out.push("nix-command flakes".into());
        out.push("--option".into());
        out.push("warn-dirty".into());
        out.push("false".into());
        out.extend(args.iter().map(|s| s.to_string()));
        out
    }

    /// Like [`Options::nix_command`], but rooted at `nix_build_bin` — which
    /// already carries the `build` subcommand — instead of `nix_bin`.
    ///
    /// This is the seam that lets a caller wrap *only the actual builds* in a
    /// resource-limited scope (`systemd-run --user --scope -p MemoryMax=…`)
    /// while leaving the cheap auxiliary invocations (`nix log`, `nix copy`,
    /// `nix flake metadata`) unwrapped. Before this existed, `nix_build_bin`
    /// was parsed from `--nix-build` and then never read.
    ///
    /// The global flags land *after* the subcommand, which `nix` accepts.
    pub fn nix_build_command(&self, args: &[&str]) -> Vec<String> {
        let mut out = Vec::with_capacity(self.nix_build_bin.len() + 5 + args.len());
        out.extend(self.nix_build_bin.iter().cloned());
        out.push("--experimental-features".into());
        out.push("nix-command flakes".into());
        out.push("--option".into());
        out.push("warn-dirty".into());
        out.push("false".into());
        out.extend(args.iter().map(|s| s.to_string()));
        out
    }

    pub fn remote_url(&self) -> Option<String> {
        self.remote.as_ref().map(|r| format!("ssh://{r}"))
    }

    /// Human-readable name for the evaluation target, used in reports.
    pub fn display_name(&self) -> String {
        match self.eval_mode {
            EvalMode::Flake => {
                if self.flake_fragment.is_empty() {
                    self.flake_url.clone()
                } else {
                    format!("{}#{}", self.flake_url, self.flake_fragment)
                }
            }
            EvalMode::Expr => {
                let mut name = self.expr_file.clone();
                if !self.expr_attr.is_empty() {
                    name.push('.');
                    name.push_str(&self.expr_attr);
                }
                name
            }
        }
    }
}

/// Wrap `cmd` in `ssh REMOTE [-o ...] -- <shlex-joined cmd>` if a remote is set.
pub fn maybe_remote(cmd: Vec<String>, opts: &Options) -> Vec<String> {
    match &opts.remote {
        Some(remote) => {
            let joined =
                shlex::try_join(cmd.iter().map(String::as_str)).expect("shell-quotable cmd");
            let mut out = vec!["ssh".to_string(), remote.clone()];
            out.extend(opts.remote_ssh_options.iter().cloned());
            out.push("--".into());
            out.push(joined);
            out
        }
        None => cmd,
    }
}

/// Equivalent of the Python `nix_shell()` helper at `__init__.py:682-684`.
/// Wraps a command so it falls back to `nix shell nixpkgs#<pkg>` when the
/// target binary is not on PATH (used to bootstrap nix-eval-jobs / nom /
/// cachix on remote builders).
pub fn nix_shell(fallback_package: &str, wanted_command: &str) -> Vec<String> {
    let bash_cmd = "pkg=$1; shift; cmd=(\"$@\"); if command -v \"${cmd[0]}\" >/dev/null; then exec \"${cmd[@]}\"; else exec nix --experimental-features \"nix-command flakes\" shell \"$pkg\" -c \"${cmd[@]}\"; fi";
    vec![
        "bash".into(),
        "-c".into(),
        bash_cmd.into(),
        "bash".into(),
        fallback_package.into(),
        wanted_command.into(),
    ]
}
