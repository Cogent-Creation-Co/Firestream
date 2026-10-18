//! Command-line parsing. The full flag surface of the Python tool is preserved
//! so call sites in `bin/ci/ci-linux.sh` keep working unchanged.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{ArgAction, Parser, ValueEnum};

use crate::nix_config;
use crate::options::{EvalMode, Options, OverrideInput, ResultFormat};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum CliResultFormat {
    Json,
    Junit,
}

impl From<CliResultFormat> for ResultFormat {
    fn from(value: CliResultFormat) -> Self {
        match value {
            CliResultFormat::Json => ResultFormat::Json,
            CliResultFormat::Junit => ResultFormat::Junit,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "firestream-nix-build",
    version,
    about = "Combine the power of nix-eval-jobs with nix-output-monitor to speed up your evaluation and building process."
)]
pub struct Cli {
    // ---- Binary paths -----------------------------------------------------
    #[arg(long, env = "NIX_FAST_BUILD_NIX", default_value = "nix")]
    pub nix: String,

    #[arg(
        long = "nix-eval-jobs",
        env = "NIX_FAST_BUILD_EVAL_JOBS",
        default_value = "nix-eval-jobs"
    )]
    pub nix_eval_jobs: String,

    #[arg(
        long = "nix-build",
        env = "NIX_FAST_BUILD_NIX_BUILD",
        default_value = "nix-build"
    )]
    pub nix_build: String,

    // ---- Evaluation mode (mutually exclusive: --flake / --file) -----------
    #[arg(short = 'f', long, group = "eval_mode")]
    pub flake: Option<String>,

    /// Nix expression file to evaluate. When given without a value, defaults
    /// to `default.nix`. Mutually exclusive with `--flake`.
    #[arg(long, group = "eval_mode", num_args = 0..=1, default_missing_value = "default.nix")]
    pub file: Option<String>,

    // ---- Non-flake specific options ---------------------------------------
    #[arg(short = 'A', long, default_value = "")]
    pub attr: String,

    /// Pass `--arg NAME VALUE` to Nix (non-flake mode, repeatable).
    #[arg(long = "arg", num_args = 2, value_names = ["NAME", "VALUE"], action = ArgAction::Append)]
    pub arg: Vec<String>,

    /// Pass `--argstr NAME VALUE` to Nix (non-flake mode, repeatable).
    #[arg(long = "argstr", num_args = 2, value_names = ["NAME", "VALUE"], action = ArgAction::Append)]
    pub argstr: Vec<String>,

    /// Add path to the Nix search path (non-flake mode, repeatable).
    #[arg(short = 'I', long = "include", action = ArgAction::Append)]
    pub include: Vec<String>,

    /// Allow impure expressions (default in `--file` mode).
    #[arg(long, action = ArgAction::SetTrue)]
    pub impure: bool,

    /// Enforce pure evaluation in `--file` mode (overrides the default impure behavior).
    #[arg(long, action = ArgAction::SetTrue)]
    pub pure: bool,

    // ---- Build control ----------------------------------------------------
    #[arg(short = 'j', long = "max-jobs")]
    pub max_jobs: Option<usize>,

    #[arg(long = "option", num_args = 2, value_names = ["NAME", "VALUE"], action = ArgAction::Append)]
    pub option: Vec<String>,

    #[arg(long = "remote-ssh-option", num_args = 2, value_names = ["NAME", "VALUE"], action = ArgAction::Append)]
    pub remote_ssh_option: Vec<String>,

    // ---- Binary cache uploads --------------------------------------------
    #[arg(long = "cachix-cache")]
    pub cachix_cache: Option<String>,

    #[arg(long = "attic-cache")]
    pub attic_cache: Option<String>,

    #[arg(long = "attic-ignore-upstream-cache-filter", action = ArgAction::SetTrue)]
    pub attic_ignore_upstream_cache_filter: bool,

    #[arg(long = "attic-push-build-closure", action = ArgAction::SetTrue)]
    pub attic_push_build_closure: bool,

    #[arg(long = "niks3-server")]
    pub niks3_server: Option<String>,

    /// Don't use nix-output-monitor. The Python tool tri-states this with the
    /// default "auto-detect if nom is on PATH"; we replicate that in
    /// `into_options()`.
    #[arg(long = "no-nom", action = ArgAction::SetTrue)]
    pub no_nom: bool,

    /// Space-separated list of systems to build for.
    #[arg(long)]
    pub systems: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub retries: u32,

    #[arg(long = "no-link", action = ArgAction::SetTrue)]
    pub no_link: bool,

    #[arg(long = "out-link", default_value = "result")]
    pub out_link: String,

    #[arg(long)]
    pub remote: Option<String>,

    #[arg(long = "always-upload-source", action = ArgAction::SetTrue)]
    pub always_upload_source: bool,

    #[arg(long = "no-download", action = ArgAction::SetTrue)]
    pub no_download: bool,

    #[arg(long = "skip-cached", action = ArgAction::SetTrue)]
    pub skip_cached: bool,

    #[arg(long = "copy-to")]
    pub copy_to: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub debug: bool,

    #[arg(long = "eval-max-memory-size", default_value_t = 4096)]
    pub eval_max_memory_size: u64,

    #[arg(long = "eval-workers", default_value_t = default_eval_workers())]
    pub eval_workers: usize,

    #[arg(long = "result-file")]
    pub result_file: Option<PathBuf>,

    /// Tee child `nix`/`nix-build` stderr to this file (append mode).
    /// Useful for recovering the actual failure cause when nix-fast-build
    /// exits before writing its JSON result file.
    #[arg(long = "stderr-log")]
    pub stderr_log: Option<PathBuf>,

    #[arg(long = "result-format", value_enum, default_value_t = CliResultFormat::Json)]
    pub result_format: CliResultFormat,

    #[arg(long = "override-input", num_args = 2, value_names = ["INPUT_PATH", "FLAKE_URL"], action = ArgAction::Append)]
    pub override_input: Vec<String>,

    /// Nix function applied to the evaluation root to filter or transform
    /// the set of attributes to build.
    #[arg(long, value_name = "NIX_FUNCTION")]
    pub select: Option<String>,

    #[arg(long = "reference-lock-file")]
    pub reference_lock_file: Option<String>,

    // ---- ConceptDB fork: OTel ingest --------------------------------------
    /// Emit per-derivation OpenTelemetry spans (ConceptDB fork, PRD §9.2).
    #[arg(long = "otel-ingest", action = ArgAction::SetTrue)]
    pub otel_ingest: bool,

    /// W3C traceparent to nest emitted build spans under.
    #[arg(long = "otel-parent-trace", env = "TRACEPARENT")]
    pub otel_parent_trace: Option<String>,

    #[arg(
        long = "otel-service",
        env = "OTEL_SERVICE_NAME",
        default_value = "firestream-ci"
    )]
    pub otel_service: String,

    /// Retained for CLI compatibility. Unused: this Rust port runs the
    /// ingest in-process and does not spawn `otel-cli`.
    #[arg(long = "otel-cli", env = "OTEL_CLI", default_value = "otel-cli")]
    pub otel_cli: String,
}

fn default_eval_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

impl Cli {
    pub async fn into_options(self) -> Result<Options> {
        // Determine evaluation mode (mirrors Python __init__.py:460).
        let eval_mode = if self.file.is_some() {
            EvalMode::Expr
        } else {
            EvalMode::Flake
        };

        // Mode-specific validation, mirroring Python __init__.py:463-487.
        if eval_mode == EvalMode::Expr {
            if self.remote.is_some() {
                bail!("--remote is not supported in non-flake (--file) mode");
            }
            if !self.override_input.is_empty() {
                bail!("--override-input is not supported in non-flake (--file) mode");
            }
            if self.always_upload_source {
                bail!("--always-upload-source is not supported in non-flake (--file) mode");
            }
        } else {
            if !self.attr.is_empty() {
                bail!("-A/--attr is only supported in non-flake (--file) mode");
            }
            if !self.arg.is_empty() {
                bail!("--arg is only supported in non-flake (--file) mode");
            }
            if !self.argstr.is_empty() {
                bail!("--argstr is only supported in non-flake (--file) mode");
            }
            if !self.include.is_empty() {
                bail!("-I/--include is only supported in non-flake (--file) mode");
            }
            if self.pure {
                bail!("--pure is only supported in non-flake (--file) mode");
            }
        }

        // In --file mode, default to impure unless --pure given.
        let impure = match eval_mode {
            EvalMode::Expr => !self.pure,
            EvalMode::Flake => self.impure,
        };

        // Parse flake mode settings.
        let (flake_url, flake_fragment) = if eval_mode == EvalMode::Flake {
            let spec = self.flake.as_deref().unwrap_or(".#checks");
            match spec.split_once('#') {
                Some((url, frag)) => (url.to_string(), frag.to_string()),
                None => (spec.to_string(), String::new()),
            }
        } else {
            (String::new(), String::new())
        };

        // Parse expression-mode settings.
        let (expr_file, expr_attr, expr_args) = if eval_mode == EvalMode::Expr {
            let mut args: Vec<String> = Vec::new();
            for pair in self.arg.chunks(2) {
                args.push("--arg".into());
                args.push(pair[0].clone());
                args.push(pair[1].clone());
            }
            for pair in self.argstr.chunks(2) {
                args.push("--argstr".into());
                args.push(pair[0].clone());
                args.push(pair[1].clone());
            }
            for path in &self.include {
                args.push("--include".into());
                args.push(path.clone());
            }
            (
                self.file.clone().unwrap_or_default(),
                self.attr.clone(),
                args,
            )
        } else {
            (String::new(), String::new(), Vec::new())
        };

        // Flatten --option NAME VALUE pairs into argv form.
        let mut options_flat: Vec<String> = Vec::new();
        for pair in self.option.chunks(2) {
            options_flat.push("--option".into());
            options_flat.push(pair[0].clone());
            options_flat.push(pair[1].clone());
        }

        // Flatten --remote-ssh-option NAME VALUE pairs into ssh -o form.
        let mut remote_ssh_flat: Vec<String> = Vec::new();
        for pair in self.remote_ssh_option.chunks(2) {
            remote_ssh_flat.push("-o".into());
            remote_ssh_flat.push(format!("{}={}", pair[0], pair[1]));
        }

        // Pair override-inputs.
        let override_inputs: Vec<OverrideInput> = self
            .override_input
            .chunks(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();

        // Tokenize binary paths (Python uses shlex.split to allow `--nix "nix --foo"`).
        let nix_bin =
            shlex::split(&self.nix).ok_or_else(|| anyhow::anyhow!("invalid --nix value"))?;
        let nix_eval_jobs_bin = shlex::split(&self.nix_eval_jobs)
            .ok_or_else(|| anyhow::anyhow!("invalid --nix-eval-jobs value"))?;
        let nix_build_bin = shlex::split(&self.nix_build)
            .ok_or_else(|| anyhow::anyhow!("invalid --nix-build value"))?;

        // Probe nix config for defaults: max-jobs and current system.
        let cfg =
            nix_config::get_nix_config(&nix_bin, self.remote.as_deref(), &remote_ssh_flat).await?;

        let max_jobs = match self.max_jobs {
            Some(n) => n,
            None => cfg
                .get("max-jobs")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0),
        };

        // The Python tool tri-states --no-nom: explicit | auto.
        // clap can't model tri-state cleanly, so we replicate auto-detect
        // *only when* the user did not pass --no-nom (the default is false).
        let nom = if self.no_nom {
            false
        } else if self.remote.is_some() {
            // Only enable nom on remote builds when the remote system is a
            // known platform with an official binary cache (otherwise we'd
            // have to build GHC there).
            let sys = cfg.get("system").map(String::as_str).unwrap_or("");
            matches!(
                sys,
                "aarch64-darwin" | "x86_64-darwin" | "aarch64-linux" | "x86_64-linux"
            )
        } else {
            // Local: enable nom only if it's on PATH.
            which("nom")
        };

        // Default systems = {current_system}.
        let systems: BTreeSet<String> = match self.systems.as_deref() {
            Some(s) => s.split_whitespace().map(String::from).collect(),
            None => {
                let mut set = BTreeSet::new();
                if let Some(sys) = cfg.get("system") {
                    set.insert(sys.clone());
                }
                set
            }
        };

        Ok(Options {
            nix_bin,
            nix_eval_jobs_bin,
            nix_build_bin,
            eval_mode,
            flake_url,
            flake_fragment,
            expr_file,
            expr_attr,
            expr_args,
            impure,
            options: options_flat,
            remote: self.remote,
            remote_ssh_options: remote_ssh_flat,
            always_upload_source: self.always_upload_source,
            systems,
            eval_max_memory_size: self.eval_max_memory_size,
            skip_cached: self.skip_cached,
            eval_workers: self.eval_workers,
            max_jobs,
            retries: self.retries,
            debug: self.debug,
            copy_to: self.copy_to,
            nom,
            download: !self.no_download,
            no_link: self.no_link,
            out_link: self.out_link,
            result_format: self.result_format.into(),
            result_file: self.result_file,
            stderr_log: self.stderr_log,
            // CLI path never exposes a ring sink (no caller to share Arc with).
            ring_sink: None,
            override_inputs,
            select_expr: self.select,
            reference_lock_file: self.reference_lock_file,
            cachix_cache: self.cachix_cache,
            attic_cache: self.attic_cache,
            attic_ignore_upstream_cache_filter: self.attic_ignore_upstream_cache_filter,
            attic_push_build_closure: self.attic_push_build_closure,
            niks3_server: self.niks3_server,
            otel_ingest: self.otel_ingest,
            otel_parent_trace: self.otel_parent_trace,
            otel_service: self.otel_service,
            otel_cli_bin: self.otel_cli,
        })
    }
}

fn which(prog: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(prog).is_file())
}
