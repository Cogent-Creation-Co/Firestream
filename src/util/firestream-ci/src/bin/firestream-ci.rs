//! `firestream-ci` binary. M3 shipped `spans`, `version`, `worktree`; M5 added
//! `limits exec`, `limits watchdog`, `oci flatten`, and `report summary`.
//! M6 adds `nix gc` and the canonical `ci-linux` phase orchestrator.
//! Later milestones extend with `ci-docker` and `ci`. Each
//! subcommand is a thin shim over the library — the only meaningful logic
//! in this binary is `ci_linux` itself (composes `Pipeline` + `nix::FastBuild`
//! + `manifest` + `report`), and even that is a glue function under 250 LoC.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use firestream_ci::artifacts::ArtifactsFormat;
use firestream_ci::checkpoint::Replayer;
use firestream_ci::exec::Command as ExecCommand;
use firestream_ci::limits::{LimitedCommand, Limits};
use firestream_ci::manifest::Outcome;
use firestream_ci::report::Report;
use firestream_ci::rundir::RunDir;
use firestream_ci::trace::Tracer;
use firestream_ci::util::log_paths::{
    nix_fast_build_log_path, nix_fast_build_result_path, stderr_log_path,
};
use firestream_ci::version::{FileKind, Manifest};
use firestream_ci::worktree::Worktree;

/// A run-scoped context that owns the per-run output directory. Every CI
/// runner (linux/darwin/docker/cloudbuild) implements this so the
/// finalize-manifest / artifact-export plumbing has one source of truth for
/// "where do files for this run live?" instead of duplicated
/// `build_output_dir` / `log_dir` / `span_dir` fields per ctx.
pub trait RunContext {
    fn rundir(&self) -> &RunDir;
}

#[derive(Parser)]
#[command(
    name = "firestream-ci",
    version,
    about = "Build-orchestration toolkit (Nix + Docker + OTel)"
)]
struct Cli {
    /// Path to the CI profile (`ci-manifest.json`), or a directory containing
    /// one — e.g. the output of `nix build .#firestream-ci-profile`.
    ///
    /// Everything project-specific this tool knows comes from here: phase
    /// definitions and their attrs, tier classification, export targets,
    /// builder-image identity, the passthrough allowlist, and the devshell
    /// sentinels. See `firestream_ci::profile`.
    ///
    /// Resolution order when omitted:
    ///   `$FIRESTREAM_CI_PROFILE` → `./ci-manifest.json`
    ///   → `/opt/firestream/ci/ci-manifest.json`
    /// A path given here short-circuits the chain: if it does not exist, that
    /// is an error rather than a fallthrough. When nothing resolves at all,
    /// a built-in project-free default is used.
    #[arg(long = "profile", global = true, value_name = "PATH")]
    profile: Option<PathBuf>,

    #[command(subcommand)]
    cmd: TopCmd,
}

#[derive(Subcommand)]
enum TopCmd {
    /// Span checkpoint operations.
    Spans {
        #[command(subcommand)]
        cmd: SpansCmd,
    },
    /// Version manifest operations.
    Version {
        #[command(subcommand)]
        cmd: VersionCmd,
    },
    /// Worktree operations.
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCmd,
    },
    /// Resource-limit wrappers (cgroup on Linux, watchdog on Darwin).
    Limits {
        #[command(subcommand)]
        cmd: LimitsCmd,
    },
    /// OCI / Docker image operations.
    Oci {
        #[command(subcommand)]
        cmd: OciCmd,
    },
    /// Span-tree → summary reports.
    Report {
        #[command(subcommand)]
        cmd: ReportCmd,
    },
    /// Nix store operations.
    Nix {
        #[command(subcommand)]
        cmd: NixCmd,
    },
    /// Kubernetes helpers (branch → namespace mapping for k3s deployments).
    K8s {
        #[command(subcommand)]
        cmd: K8sCmd,
    },
    /// Build-strategy predicate: native `nix build` vs the Docker builder.
    ///
    /// `firestream_ci::platform` is the single authority; `bin/build/strategy.sh`
    /// is its zero-dependency shell mirror and `bin/build/strategy-cases.json`
    /// the golden-vector parity gate between them. This subcommand is that
    /// authority's CLI surface.
    Platform {
        #[command(subcommand)]
        cmd: PlatformCmd,
    },
    /// Container-image and fleet-SBOM builds. The typed equivalent of
    /// `bin/build/container-images.sh` and `bin/build/manifest.sh`.
    ///
    /// STRANGLER STATUS: those two scripts remain the DEFAULT and remain
    /// fully working. This path is opt-in — `FIRESTREAM_BUILD_IMPL=rust`, or
    /// `--rust` on either script, or `make <target> IMPL=rust`. It becomes
    /// the default only after it has been observed to build a real image on
    /// both a Linux and a Darwin host (see the deletion checklist in the
    /// scripts' headers).
    ///
    /// The native-vs-docker decision comes from `firestream_ci::platform`,
    /// i.e. the same authority `bin/build/strategy.sh` mirrors, so
    /// `FIRESTREAM_BUILD_STRATEGY` behaves identically on both paths.
    Build {
        #[command(subcommand)]
        cmd: BuildCmd,
    },

    /// Repo-local disk hygiene: `cargo sweep` over `target/` (root workspace
    /// + the isolated `src/util` workspace) and prune `_build/` CI rundirs.
    ///
    /// Host-safe by construction — it only ever deletes under the repo's own
    /// `target/` and `_build/`, never `/nix/store` — so unlike `nix gc` it is
    /// deliberately NOT gated on RUNNING_IN_DOCKER. The CI tidy phase runs
    /// the same logic as its `target-sweep` task in every mode.
    Sweep(SweepArgs),
    /// Canonical Linux CI phase orchestrator. Replaces `bin/ci/ci-linux.sh`.
    ///
    /// Designed to run inside the firestream-builder container (when
    /// ci-docker.sh invokes it) OR directly on a Linux host inside
    /// `nix develop` (CI_RUNNER=host-linux). The env-var contract is
    /// identical to the bash version; flags exist for explicit overrides.
    CiLinux(CiLinuxArgs),

    /// Top-level CI dispatcher. Replaces `bin/ci/ci.sh`.
    ///
    /// Reads `CI_RUNNER` (or auto-detects via `runner::Runner::detect`),
    /// then dispatches to one of `ci-linux`, `ci-docker`,
    /// `ci-cloudbuild`. Mirrors the bash auto-detect order exactly:
    /// Cloud Build env → Darwin → Linux+Nix+devshell → Docker.
    Ci(CiDispatchArgs),

    /// CI pipeline runner for the Docker harness. Replaces
    /// `bin/ci/ci-docker.sh`. Resolves builder image, creates container,
    /// copies source via `oci::source_sync`, runs `firestream-ci ci-linux` inside,
    /// flattens the container into a single-layer warm-cache image via
    /// `oci::flatten`. Env-var contract preserved (AR_REGISTRY, BRANCH,
    /// CI_MODE, FORCE, CONTAINER_ARCH).
    CiDocker(CiDockerArgs),

    /// CI pipeline runner for Cloud Build. Replaces
    /// `bin/ci/ci-cloudbuild.sh`. Thin policy bundle: asserts BUILD_ID,
    /// applies Cloud Build defaults (DOCKER_MEMORY, OTEL_REPLAY_REQUIRED),
    /// delegates to `firestream-ci ci-docker`.
    CiCloudbuild(CiCloudbuildArgs),
}

#[derive(Subcommand)]
enum SpansCmd {
    /// Replay orphaned background spans from a checkpoint directory.
    Replay(SpansReplayArgs),
}

#[derive(Args)]
struct SpansReplayArgs {
    /// Checkpoint directory to scan. Falls back to `OTEL_CHECKPOINT_DIR`.
    #[arg(long = "checkpoint-dir")]
    checkpoint_dir: Option<PathBuf>,

    /// Only process run files at least this many seconds old. Default: 0.
    #[arg(long = "min-age", default_value_t = 0)]
    min_age_seconds: u64,

    /// Build the OTel client from the ambient env. Default behaviour;
    /// pass explicitly for clarity at the call site.
    #[arg(long = "client-from-env")]
    client_from_env: bool,
}

#[derive(Subcommand)]
enum VersionCmd {
    /// Check that every listed file holds the expected semver.
    Check(VersionCheckArgs),
}

#[derive(Args)]
struct VersionCheckArgs {
    /// One `--file <path>` per file to check. Type is auto-detected from
    /// the file name (`Cargo.toml`, `package.json`, `pyproject.toml`).
    #[arg(long = "file")]
    files: Vec<PathBuf>,

    /// Expected semver value (e.g. `1.2.3`).
    #[arg(long = "expect")]
    expect: String,
}

#[derive(Subcommand)]
enum WorktreeCmd {
    /// Compute the bind-mount set libgit2 needs to operate on a worktree
    /// from inside a container.
    Mounts(WorktreeMountsArgs),
}

#[derive(Args)]
struct WorktreeMountsArgs {
    /// Path to the worktree (default: `.`).
    #[arg(long = "repo", default_value = ".")]
    repo: PathBuf,
}

#[derive(Subcommand)]
enum LimitsCmd {
    /// Wrap a child command with cgroup limits (Linux: systemd-run --user
    /// --scope) or fall through if systemd is unavailable. On macOS, spawns
    /// a polling watchdog around the child for the duration of the run.
    /// Replaces `bin/ci/run-with-limits-linux.sh`.
    Exec(LimitsExecArgs),

    /// Spawn a 1 Hz process-tree memory watchdog for an existing PID.
    /// Replaces `bin/ci/memory-watchdog-darwin.sh`.
    Watchdog(LimitsWatchdogArgs),
}

#[derive(Args)]
struct LimitsExecArgs {
    /// Hard memory ceiling in megabytes. Bash equivalent: `LIMITS_MEMORY_MAX`
    /// (e.g. `14G` ⇒ `14336`).
    #[arg(long = "max-memory")]
    max_memory_mb: Option<u64>,

    /// Soft memory ceiling in megabytes. Bash equivalent: `LIMITS_MEMORY_HIGH`
    /// (e.g. `12G` ⇒ `12288`).
    #[arg(long = "soft-memory")]
    soft_memory_mb: Option<u64>,

    /// systemd `IOWeight=` value. Bash default: 200. NOTE: proportional IO
    /// weighting is a silent no-op on none-scheduler NVMe (needs bfq or an
    /// io.cost model) — use `--io-write-max`/`--io-read-max` for a cap that
    /// actually binds.
    #[arg(long = "io-weight")]
    io_weight: Option<u32>,

    /// Absolute write-bandwidth cap in MB/s (systemd `IOWriteBandwidthMax=`,
    /// cgroup io.max). The knob that prevents a runaway cargo/nix build from
    /// freezing the host.
    #[arg(long = "io-write-max")]
    io_write_max_mb: Option<u64>,

    /// Absolute read-bandwidth cap in MB/s (systemd `IOReadBandwidthMax=`).
    #[arg(long = "io-read-max")]
    io_read_max_mb: Option<u64>,

    /// Filesystem path identifying the block device the io.max caps apply to
    /// (systemd resolves the path to its backing device).
    #[arg(long = "io-device", default_value = "/")]
    io_device: String,

    /// CPU core hint (currently informational — `TasksMax=infinity`).
    #[arg(long = "cpu-weight")]
    cpu_cores: Option<u32>,

    /// Hard CPU ceiling as a percentage of one core (systemd `CPUQuota=`;
    /// 600 = six cores). Unlike `--cpu-weight`, this binds. Linux only —
    /// accepted and ignored on macOS (the watchdog has no cgroups).
    #[arg(long = "cpu-quota")]
    cpu_quota_pct: Option<u32>,

    /// The child command and its arguments. Everything after `--` is the
    /// child's argv.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    argv: Vec<String>,
}

#[derive(Args)]
struct LimitsWatchdogArgs {
    /// Root PID to poll. The watchdog walks descendants of this PID.
    #[arg(long = "pid")]
    pid: u32,

    /// Soft threshold in megabytes (bash default: 12000).
    #[arg(long = "soft", default_value_t = 12000)]
    soft_mb: u64,

    /// Hard threshold in megabytes (bash default: 14000).
    #[arg(long = "hard", default_value_t = 14000)]
    hard_mb: u64,
}

#[derive(Subcommand)]
enum OciCmd {
    /// Run the full warm-cache flatten pipeline (pending-tag → export/import
    /// → size-validate → atomic-retag → reap-orphans). Replaces the
    /// flatten portion of `bin/ci/ci-gc.sh` / `commit_flatten_builder`.
    Flatten(OciFlattenArgs),
}

#[derive(Args)]
struct OciFlattenArgs {
    /// Container name to flatten (stopped is fine).
    #[arg(long = "container")]
    container: String,

    /// Target canonical tag (e.g. `firestream-builder:main-x86_64`).
    #[arg(long = "tag")]
    tag: String,

    /// Source image the container was created from. Optional — used to
    /// re-apply ENV/WORKDIR/USER on import.
    #[arg(long = "src-image")]
    src_image: Option<String>,

    /// Bare image name for reaper scoping (e.g. `firestream-builder`).
    /// Defaults to the profile's `builder.image_name`.
    #[arg(long = "image-name")]
    image_name: Option<String>,

    /// Current target arch (e.g. `x86_64`).
    #[arg(long = "arch")]
    arch: String,

    /// Comma-separated full arch list for reaper preservation
    /// (e.g. `x86_64,aarch64`).
    #[arg(long = "known-arches", default_value = "x86_64,aarch64")]
    known_arches: String,

    /// Minimum acceptable image size in bytes. Defaults to the profile's
    /// `builder.min_image_size_bytes` (whose own default is 100 MiB,
    /// `firestream_ci::oci::flatten::MIN_BUILDER_IMAGE_SIZE_BYTES`).
    #[arg(long = "min-size-bytes")]
    min_size_bytes: Option<u64>,

    /// Lineage label: branch.
    #[arg(long = "lineage-branch", default_value = "")]
    lineage_branch: String,

    /// Lineage label: git sha.
    #[arg(long = "lineage-sha", default_value = "")]
    lineage_sha: String,

    /// Lineage label: epoch (seconds).
    #[arg(long = "lineage-epoch", default_value = "")]
    lineage_epoch: String,

    /// Lineage label: parent sha.
    #[arg(long = "lineage-parent-sha", default_value = "")]
    lineage_parent_sha: String,

    /// Lineage label: trace id.
    #[arg(long = "lineage-trace-id", default_value = "")]
    lineage_trace_id: String,
}

#[derive(Subcommand)]
enum ReportCmd {
    /// Read a span directory, print phase + build tables, and optionally
    /// write a markdown summary. Replaces `bin/ci/ci-tail.sh::ci_summary_emit`.
    Summary(ReportSummaryArgs),
}

#[derive(Args)]
struct ReportSummaryArgs {
    /// Span directory to scan. Falls back to `OTEL_SPAN_DIR`.
    #[arg(long = "span-dir")]
    span_dir: Option<PathBuf>,

    /// A CI run directory, i.e. `--span-dir <rundir>/spans`. The rundir is
    /// what every other part of the toolkit prints and what the user has in
    /// hand after a run, so accepting it directly removes a `/spans` the
    /// caller would otherwise have to remember. Wins over `--span-dir`.
    #[arg(long = "rundir")]
    rundir: Option<PathBuf>,

    /// Optional output path. Bash default for markdown:
    /// `${BUILD_OUTPUT_DIR}/profiles/ci-summary.md`. For `--format json`,
    /// any path the caller chooses; the writer is atomic.
    #[arg(long = "output")]
    output: Option<PathBuf>,

    /// Suppress the terminal tables (useful when only writing markdown).
    /// Ignored for `--format json` — JSON output is always
    /// pipe-friendly (single document on stdout, nothing else).
    #[arg(long = "no-print")]
    no_print: bool,

    /// Output format. `markdown` writes a human-readable summary table
    /// (default); `json` writes a machine-readable document suitable
    /// for `jq` or remote ingest.
    #[arg(long = "format", value_enum, default_value_t = ReportFormat::Markdown)]
    format: ReportFormat,
}

/// Output format selector for `firestream-ci report summary`. Mirrors
/// `ArtifactsFormat` (Phase 4) so the CLI surface is uniform.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum ReportFormat {
    /// Phase + build tables in markdown (default; back-compat).
    Markdown,
    /// Versioned, `jq`-shaped JSON document. See
    /// `Report::to_json` for the on-disk schema.
    Json,
}

/// Output mode for `ci` / `ci-linux`. `json` is "agent mode": stdout carries
/// only newline-delimited typed `firestream.ci.v1.Event` frames (the rundir + the
/// structured verdict), human progress stays on stderr.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Human progress on stderr; nothing structured on stdout (default).
    #[default]
    Human,
    /// Typed JSON-over-stdio frames on stdout (see `wire/`).
    Json,
}

#[derive(Subcommand)]
enum NixCmd {
    /// Roots-based Nix store GC. Mirrors `bin/ci/ci-gc.sh`: enumerate every
    /// `checks.<sys>.*` and `packages.<sys>.*` attribute, register each as
    /// an indirect GC root, then run `nix-collect-garbage`. Refuses to run
    /// outside the builder container unless `--allow-host` is supplied.
    Gc(NixGcArgs),
}

#[derive(Args)]
struct NixGcArgs {
    /// Path to the flake (default: `.`).
    #[arg(long = "flake-path", default_value = ".")]
    flake_path: PathBuf,

    /// Nix system (e.g. `x86_64-linux`). Falls back to `NIX_SYSTEM`. If
    /// neither is set, infers from `builtins.currentSystem` via `nix eval`.
    #[arg(long = "system")]
    system: Option<String>,

    /// Directory under which to materialise the indirect GC roots. Default
    /// matches bash: `/tmp/ci-gc-roots`.
    #[arg(long = "roots-dir", default_value = "/tmp/ci-gc-roots")]
    roots_dir: PathBuf,

    /// `nix-store --option max-jobs` value (bash default: 1).
    #[arg(long = "max-jobs", default_value_t = 1)]
    max_jobs: u32,

    /// `nix-store --option cores` value (bash default: 2).
    #[arg(long = "cores", default_value_t = 2)]
    cores: u32,

    /// Force GC on the host store. Mirrors the bash bail-out: the script
    /// refuses to run when `RUNNING_IN_DOCKER` is unset because the host
    /// /nix/store is not the per-image store; deleting roots there would
    /// destroy a developer's working store. Set this flag (or
    /// `RUNNING_IN_DOCKER=1`) to override.
    #[arg(long = "allow-host")]
    allow_host: bool,

    /// Keep the newest N rundirs under `_build/`. Older rundirs are pruned
    /// (respecting the 60-s sentinel grace window for concurrent runs).
    /// Default: 20 — matches the bash pattern.
    #[arg(long = "keep-rundirs", default_value_t = 20)]
    keep_rundirs: usize,

    /// Skip the one-time scrub of legacy roots (`_build/spans/`,
    /// `_build/logs/`, `_build/watchdog/`). Default: enabled — the legacy
    /// roots are dead once spans/logs land inside rundirs, but a paranoid
    /// operator can opt out with this flag.
    #[arg(long = "no-legacy-cleanup", default_value_t = false)]
    no_legacy_cleanup: bool,
}

#[derive(Args)]
struct SweepArgs {
    /// Age threshold in days for `cargo sweep --time`: artifacts whose
    /// fingerprints haven't been used within this window are removed.
    /// A `cargo sweep --installed` pass (artifacts orphaned by toolchain
    /// bumps) always runs alongside it.
    #[arg(long = "days", default_value_t = 7)]
    days: u64,

    /// Keep the newest N rundirs under `_build/` (same semantics as
    /// `nix gc --keep-rundirs`).
    #[arg(long = "keep-rundirs", default_value_t = 20)]
    keep_rundirs: usize,

    /// Report what would be removed without deleting anything.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Repo root to sweep (default: current directory).
    #[arg(long = "repo-root")]
    repo_root: Option<PathBuf>,

    /// Sweep every worktree of this repo (`git worktree list` from
    /// --repo-root), not just --repo-root itself. Roots without a `target/`
    /// are skipped.
    #[arg(long = "all-worktrees")]
    all_worktrees: bool,

    /// Per-worktree size budget for `target/` in GiB. When the age-based
    /// sweep leaves a target/ above this, escalate: re-sweep with smaller
    /// windows, then drop `target/*/incremental` (pure cache; worst case is
    /// one warm rebuild of recently edited crates).
    #[arg(long = "max-target-gb")]
    max_target_gb: Option<u64>,

    #[command(subcommand)]
    cmd: Option<SweepSubcmd>,
}

#[derive(Subcommand)]
enum SweepSubcmd {
    /// Manage the user-level scheduled sweep (systemd user timer on Linux,
    /// launchd agent on macOS) so target/ stays bounded with zero manual GC.
    Timer {
        #[command(subcommand)]
        cmd: SweepTimerCmd,
    },
}

#[derive(Subcommand)]
enum SweepTimerCmd {
    /// Install (or refresh) the nightly sweep unit. Idempotent: re-renders
    /// and only rewrites/reloads when the rendered units differ, so it is
    /// safe to run on every devshell entry.
    Install(SweepTimerInstallArgs),
    /// Disable the timer and remove the unit files.
    Uninstall,
    /// Show timer state and recent journal output.
    Status,
}

#[derive(Args)]
struct SweepTimerInstallArgs {
    /// Age window baked into the scheduled sweep.
    #[arg(long = "days", default_value_t = 7)]
    days: u64,

    /// Per-worktree target/ budget baked into the scheduled sweep.
    #[arg(long = "max-target-gb", default_value_t = 250)]
    max_target_gb: u64,

    /// `_build/` rundirs kept by the scheduled sweep.
    #[arg(long = "keep-rundirs", default_value_t = 20)]
    keep_rundirs: usize,

    /// Repo root baked into the unit (default: current directory). The
    /// scheduled sweep runs with --all-worktrees from here.
    #[arg(long = "repo-root")]
    repo_root: Option<PathBuf>,
}

#[derive(Subcommand)]
enum BuildCmd {
    /// Build one or more container images and `docker load` them.
    /// Mirrors `bin/build/container-images.sh`.
    Images(BuildImagesArgs),
    /// Build the fleet manifest (no argument) or one container's SBOM.
    /// Mirrors `bin/build/manifest.sh`.
    Manifest(BuildManifestArgs),
    /// Print the Nix package name a container+version resolves to, and exit.
    ///
    /// The parity probe for the registry table: this is the Rust side of
    /// `bash -c 'source bin/build/_common.sh; resolve_package_name redis ""'`,
    /// and `bin/build/test-registry-parity.sh` gates both against
    /// `bin/build/registry-cases.json`.
    Resolve(BuildResolveArgs),
}

/// `firestream-ci build images ...` takes its argv **raw** and parses it with a
/// hand-written left-to-right loop that is a line-by-line mirror of
/// `container-images.sh`'s `while [[ $# -gt 0 ]]` — see [`parse_images_argv`].
///
/// Clap is deliberately not used for the flags here. The shell's `--version`
/// is a *latch* that applies to the NEXT positional, which clap has no way to
/// express, and the whole value of this phase is that the two paths accept the
/// same command line and mean the same thing by it.
#[derive(Args)]
struct BuildImagesArgs {
    /// Everything `container-images.sh` accepts: container names,
    /// `--version <v>` (applies to the NEXT container), `--target <arch>`,
    /// `--native`, `--docker`. Plus this path's own `--dry-run`,
    /// `--no-load`, `--repo-root <p>`, `--build-output-dir <p>`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    argv: Vec<String>,
}

#[derive(Args)]
struct BuildManifestArgs {
    /// Everything `manifest.sh` accepts: an optional container name,
    /// `--native`, `--docker`. Plus `--dry-run`, `--target <arch>`,
    /// `--repo-root <p>`, `--build-output-dir <p>`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    argv: Vec<String>,
}

/// Result of the hand-written argv parse, shared by both build subcommands.
#[derive(Debug, Default)]
struct ParsedBuildArgv {
    /// (container, version) pairs, in argv order.
    containers: Vec<(String, String)>,
    target: Option<String>,
    /// `native` / `docker` — written into `FIRESTREAM_BUILD_STRATEGY` exactly
    /// as the shell's `--native` / `--docker` do, so the SAME predicate
    /// resolves it and there is no second code path.
    strategy: Option<&'static str>,
    repo_root: Option<PathBuf>,
    build_output_dir: Option<PathBuf>,
    no_load: bool,
    dry_run: bool,
    /// A `--version` that no positional ever consumed. The shell silently
    /// drops it; we keep it so the caller can WARN, because
    /// `makefile:544 redis-build-%` is exactly this shape and it means
    /// `make redis-build-8` builds redis-7.
    dangling_version: Option<String>,
}

#[derive(Args)]
struct BuildResolveArgs {
    /// Container family (`redis`, `postgresql`, …).
    container: String,
    /// Version. Omit for the family default — which is NOT always the newest:
    /// bare `redis` is redis-7 while `.#redis` in the flake is redis-8.
    version: Option<String>,
}

#[derive(Subcommand)]
enum PlatformCmd {
    /// Decide how a build for `--target` should execute here, and say why.
    ///
    /// Exit 0 always — this is a query, not a gate. The strategy goes to
    /// stdout on its own line (so `$(firestream-ci platform decide …)` is the
    /// value), the reasoning to stderr.
    Decide(PlatformDecideArgs),
    /// Dump the raw probe (`uname`, nix/docker availability, container
    /// detection) as JSON. The same struct the golden-vector parity test
    /// feeds fixture values into.
    Probe,
}

#[derive(Args)]
struct PlatformDecideArgs {
    /// Target architecture (`x86_64`, `aarch64`, or a Docker-style alias like
    /// `amd64` / `arm64`). Defaults to the host's.
    #[arg(long = "target")]
    target: Option<String>,

    /// Emit the full [`Decision`] as JSON instead of the bare strategy word.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Subcommand)]
enum K8sCmd {
    /// Print the k3s namespace for the current branch (mirrors the bash
    /// `resolve_branch_vars` namespace formula: `firestream-<safe-branch>`
    /// where `<safe-branch>` is lowercase, non-alphanum→`-`, clipped to 53
    /// chars). Used by the `logs-k3s` and `status-k3s` Makefile targets.
    Namespace,
}

#[derive(Args)]
struct CiLinuxArgs {
    /// `check` or `release`. Falls back to `CI_MODE` env var, default
    /// `release`. Matches `bin/ci/ci-linux.sh:34`.
    #[arg(long = "mode")]
    mode: Option<String>,

    /// Nix system (e.g. `x86_64-linux`). Falls back to `NIX_SYSTEM` (the
    /// bash requires this be exported by the Makefile, line 217).
    #[arg(long = "nix-system")]
    nix_system: Option<String>,

    /// Container/target arch (`x86_64`, `aarch64`). Falls back to
    /// `HOST_ARCH`, then `uname -m`. Mirrors `bin/ci/ci-linux.sh:442`.
    #[arg(long = "arch")]
    arch: Option<String>,

    /// Per-run build output dir. Falls back to `BUILD_OUTPUT_DIR`.
    /// Inside docker this is exported by `ci-docker.sh`; on a host runner
    /// the binary allocates a fresh one (mirrors lines 41-46).
    #[arg(long = "build-output-dir")]
    build_output_dir: Option<PathBuf>,

    /// Span directory. Falls back to `OTEL_SPAN_DIR`, then
    /// `<build-output-dir>/spans`. Mirrors `bin/ci/ci-linux.sh:285`.
    #[arg(long = "otel-span-dir")]
    otel_span_dir: Option<PathBuf>,

    /// CI runner label. Falls back to `CI_RUNNER`. When set to anything
    /// other than `docker` the guard at line 26 requires the Nix devshell.
    #[arg(long = "ci-runner")]
    ci_runner: Option<String>,

    /// Skip the tidy phase (Nix GC). Mirrors `RUNNING_IN_DOCKER != 1` skip
    /// path when run outside docker. Useful for local dry-runs.
    #[arg(long = "skip-gc")]
    skip_gc: bool,

    /// How artifacts are materialized into <rundir>/artifacts/. Copy is the
    /// safe default; hardlink saves disk on single-FS deployments but
    /// silently falls back to copy across filesystems; symlink is fastest
    /// but breaks if the Nix store is GC'd.
    #[arg(long = "artifacts-format", default_value = "copy", value_enum)]
    artifacts_format: ArtifactsFormat,

    /// Maximum bytes per artifact before skipping the copy. Skipped
    /// artifacts still get a manifest entry with skipped_reason="size_cap".
    /// Default 2 GiB captures container images including the GPU server.
    #[arg(long = "artifacts-max-bytes", default_value_t = 2 * 1024 * 1024 * 1024)]
    artifacts_max_bytes: u64,

    /// Disable automatic OTLP replay to Honeycomb after a successful run.
    /// Default: replay fires when `HONEYCOMB_API_KEY` is set in env. Without
    /// the key, this flag is a no-op and the run is byte-identical to the
    /// offline path. Spans land in the configured Honeycomb dataset
    /// (set via `HONEYCOMB_DATASET`).
    #[arg(long = "no-export-honeycomb", default_value_t = false)]
    no_export_honeycomb: bool,

    /// Force OTLP replay to Honeycomb even when the pipeline failed. Off by
    /// default — only `outcome=passed` runs are shipped automatically. Has no
    /// effect when `HONEYCOMB_API_KEY` is unset.
    #[arg(long = "export-honeycomb-on-failure", default_value_t = false)]
    export_honeycomb_on_failure: bool,

    /// `human` (default) or `json`. In `json` ("agent mode") stdout carries
    /// only newline-delimited `firestream.ci.v1.Event` frames; all human progress
    /// goes to stderr. Lets a caller drive a run and consume a structured
    /// verdict (rundir, exit code, failing attrs + log paths).
    #[arg(long = "output-format", default_value = "human", value_enum)]
    output_format: OutputFormat,

    /// Resolve and print the CI profile — phase DAG, tiers, per-phase attrs,
    /// export rules, passthrough set — then exit 0 without running anything.
    ///
    /// This is the verification hook for the profile contract: it proves the
    /// manifest loads, validates, and expands, with no builds, no Docker and
    /// no devshell required.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(Args)]
struct CiDispatchArgs {
    /// Force a runner: `host-linux`, `host-darwin`, `docker`, `cloudbuild`.
    /// Overrides `CI_RUNNER` env var and auto-detection. Mirrors the bash
    /// dispatcher's `CI_RUNNER` env-var override.
    #[arg(long = "runner")]
    runner: Option<String>,

    /// Don't actually exec the chosen runner — just print which runner
    /// would run and return success. Useful for testing the dispatcher
    /// in isolation.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// `human` (default) or `json` — "agent mode". Forwarded to the chosen
    /// sub-runner so `firestream-ci ci --output-format json` streams typed frames.
    /// Only the `host-linux` runner emits frames today; other runners accept
    /// the flag for forward-compat.
    #[arg(long = "output-format", default_value = "human", value_enum)]
    output_format: OutputFormat,

    /// Extra arguments forwarded verbatim to the chosen sub-runner.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forward: Vec<String>,
}

#[derive(Args)]
struct CiDockerArgs {
    /// `check` or `release`. Falls back to `CI_MODE` env var, default
    /// `release`. Matches `bin/ci/ci-docker.sh:45`.
    #[arg(long = "mode")]
    mode: Option<String>,

    /// Container/target arch (`x86_64`, `aarch64`). Falls back to
    /// `CONTAINER_ARCH`, then `uname -m`. Matches lines 47-50.
    #[arg(long = "arch")]
    arch: Option<String>,

    /// Skip the warm-cache flatten/commit step on success. Useful for
    /// local debugging or smoke testing — the bash has no flag for this.
    #[arg(long = "skip-flatten")]
    skip_flatten: bool,

    /// Skip auto-pushing the flattened image to AR_REGISTRY on
    /// main/nightly. Default behaviour preserved (push when configured).
    #[arg(long = "skip-push")]
    skip_push: bool,

    /// How artifacts are materialized into <rundir>/artifacts/. See
    /// `firestream-ci ci-linux --help` for tradeoffs. Forwarded to the inner
    /// ci-linux invocation.
    #[arg(long = "artifacts-format", default_value = "copy", value_enum)]
    artifacts_format: ArtifactsFormat,

    /// Maximum bytes per artifact before skipping the copy. Forwarded to
    /// the inner ci-linux. Default 2 GiB.
    #[arg(long = "artifacts-max-bytes", default_value_t = 2 * 1024 * 1024 * 1024)]
    artifacts_max_bytes: u64,

    /// Disable automatic OTLP replay to Honeycomb after a successful run.
    /// Default: replay fires when `HONEYCOMB_API_KEY` is set in env. Without
    /// the key, this flag is a no-op and the run is byte-identical to the
    /// offline path. Forwarded to the inner ci-linux invocation. Spans land
    /// in the configured Honeycomb dataset (`HONEYCOMB_DATASET`).
    #[arg(long = "no-export-honeycomb", default_value_t = false)]
    no_export_honeycomb: bool,

    /// Force OTLP replay to Honeycomb even when the pipeline failed. Off by
    /// default. Forwarded to the inner ci-linux invocation. Has no effect
    /// when `HONEYCOMB_API_KEY` is unset.
    #[arg(long = "export-honeycomb-on-failure", default_value_t = false)]
    export_honeycomb_on_failure: bool,

    /// After a successful build, `docker cp` the run-dir out of the build
    /// container into the host BUILD_OUTPUT_DIR. On Cloud Build the `_build`
    /// bind mount may not share back to the orchestration step, so the copy
    /// guarantees artifacts (and the manifest) survive into later build steps
    /// for publishing. Set automatically by `ci-cloudbuild`; a no-op on a dev
    /// host where the bind mount already shares.
    #[arg(long = "copy-out-artifacts", default_value_t = false)]
    copy_out_artifacts: bool,
}

#[derive(Args)]
struct CiCloudbuildArgs {
    /// Forwarded to `ci-docker` after policy defaults are applied.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forward: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Every nix child this process spawns must run with the canonical
    // resource-enforcement settings (use-cgroups, keep-failed). Apply once
    // here so both FastBuild subprocesses and the direct nix-store /
    // nix-collect-garbage calls inherit them — and so the inner firestream-ci spawned
    // inside the docker builder re-applies them at its own startup. Ported
    // from the bash `export NIX_CONFIG="$(nix_config_string)"` in bin/_lib.sh.
    firestream_ci::nix::apply_canonical_nix_config();
    match dispatch(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("firestream-ci: {e}");
            ExitCode::from(1)
        }
    }
}

async fn dispatch(cli: Cli) -> Result<u8, firestream_ci::Error> {
    // Resolve the profile ONCE, at the process boundary, and hand an
    // `Arc<Profile>` to whichever subcommand needs it. A missing profile
    // degrades to the built-in project-free default (so `version check`,
    // `worktree mounts`, etc. keep working in a bare checkout); a profile that
    // exists but is malformed is a hard error here rather than a surprise
    // three phases into a run.
    //
    // EXCEPTION, and it is the important one: `ci-linux` uses the STRICT
    // resolver. `resolve_or_default` degrading a missing profile to the
    // built-in empty default is right for `version check`; for the pipeline
    // entrypoint it means a typo'd `--profile`, an unbuilt
    // `firestream-ci-profile`, or a `FIRESTREAM_CI_PROFILE` pointing at a
    // deleted store path all produce a run with zero phases that exits green.
    let profile_path = firestream_ci::profile::resolve_path(cli.profile.as_deref());
    let strict = matches!(cli.cmd, TopCmd::CiLinux(_));
    let profile = std::sync::Arc::new(if strict {
        firestream_ci::profile::resolve(cli.profile.as_deref())?
    } else {
        firestream_ci::profile::resolve_or_default(cli.profile.as_deref())?
    });

    // Re-export the resolved path so every child this process spawns —
    // `ci` re-invoking itself as `ci-linux`, and the `ci-linux` running inside
    // the docker builder — resolves the SAME profile without threading
    // `--profile` through three argv layers. Same trick as `CI_RUNNER` below.
    if let Some(p) = profile_path.as_ref() {
        std::env::set_var(firestream_ci::profile::PROFILE_ENV, p);
    }

    match cli.cmd {
        TopCmd::Spans { cmd } => match cmd {
            SpansCmd::Replay(args) => spans_replay(args).await,
        },
        TopCmd::Version { cmd } => match cmd {
            VersionCmd::Check(args) => version_check(args),
        },
        TopCmd::Worktree { cmd } => match cmd {
            WorktreeCmd::Mounts(args) => worktree_mounts(args),
        },
        TopCmd::Limits { cmd } => match cmd {
            LimitsCmd::Exec(args) => limits_exec(args).await,
            LimitsCmd::Watchdog(args) => limits_watchdog(args).await,
        },
        TopCmd::Oci { cmd } => match cmd {
            OciCmd::Flatten(args) => oci_flatten(args, &profile).await,
        },
        TopCmd::Report { cmd } => match cmd {
            ReportCmd::Summary(args) => report_summary(args),
        },
        TopCmd::Nix { cmd } => match cmd {
            NixCmd::Gc(args) => nix_gc(args).await,
        },
        TopCmd::K8s { cmd } => match cmd {
            K8sCmd::Namespace => k8s_namespace(&profile),
        },
        TopCmd::Platform { cmd } => match cmd {
            PlatformCmd::Decide(args) => platform_decide(args, &profile),
            PlatformCmd::Probe => platform_probe(),
        },
        TopCmd::Build { cmd } => match cmd {
            BuildCmd::Images(args) => build_images(args, &profile).await,
            BuildCmd::Manifest(args) => build_manifest(args, &profile).await,
            BuildCmd::Resolve(args) => build_resolve(args, &profile),
        },
        TopCmd::Sweep(args) => sweep_cmd(args).await,
        TopCmd::CiLinux(args) => ci_linux(args, profile).await,
        TopCmd::Ci(args) => ci_dispatch(args, &profile).await,
        TopCmd::CiDocker(args) => ci_docker(args, &profile).await,
        TopCmd::CiCloudbuild(args) => ci_cloudbuild(args).await,
    }
}

async fn spans_replay(args: SpansReplayArgs) -> Result<u8, firestream_ci::Error> {
    let mut b = Replayer::builder().min_age(Duration::from_secs(args.min_age_seconds));
    if let Some(dir) = args.checkpoint_dir {
        b = b.checkpoint_dir(dir);
    }
    if args.client_from_env {
        b = b.from_env();
    }
    let replayer = b.build()?;
    let dir = replayer.checkpoint_dir().display().to_string();
    let report = replayer.run().await?;
    eprintln!(
        "firestream-ci spans replay: dir={} scanned={} sent={} failed={} rotated={}",
        dir, report.scanned, report.sent, report.failed, report.rotated,
    );
    Ok(0)
}

fn version_check(args: VersionCheckArgs) -> Result<u8, firestream_ci::Error> {
    if args.files.is_empty() {
        eprintln!("firestream-ci version check: no --file specified");
        return Ok(0);
    }
    let mut m = Manifest::new();
    for f in &args.files {
        let kind = file_kind_from_path(f);
        m = m.add(kind, f);
    }
    let report = m.check(&args.expect)?;
    if report.ok() {
        eprintln!(
            "firestream-ci version check: all {} files match {}",
            report.matches.len(),
            args.expect
        );
        Ok(0)
    } else {
        for (path, actual) in &report.mismatches {
            eprintln!(
                "firestream-ci version check: MISMATCH {} (expected {}, got {})",
                path.display(),
                args.expect,
                actual
            );
        }
        Ok(1)
    }
}

fn file_kind_from_path(p: &std::path::Path) -> FileKind {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "Cargo.toml" => FileKind::Cargo,
        "package.json" => FileKind::PackageJson,
        "pyproject.toml" => FileKind::Pyproject,
        // Fall back by extension where the filename isn't canonical.
        _ if name.ends_with(".toml") && name.contains("pyproject") => FileKind::Pyproject,
        _ if name.ends_with(".toml") => FileKind::Cargo,
        _ if name.ends_with(".json") => FileKind::PackageJson,
        _ => FileKind::Cargo,
    }
}

fn worktree_mounts(args: WorktreeMountsArgs) -> Result<u8, firestream_ci::Error> {
    let wt = Worktree::open(&args.repo)?;
    let mounts = wt.container_mounts()?;
    println!("# firestream-ci worktree mounts ({})", args.repo.display());
    for m in &mounts {
        println!(
            "{}\t{}\t{:?}",
            m.source.display(),
            m.target.display(),
            m.kind
        );
    }
    Ok(0)
}

async fn limits_exec(args: LimitsExecArgs) -> Result<u8, firestream_ci::Error> {
    if args.argv.is_empty() {
        eprintln!("firestream-ci limits exec: no child command specified");
        return Ok(2);
    }

    let mut limits = Limits::default();
    if let Some(mb) = args.max_memory_mb {
        limits = limits.with_mem_max_mb(mb);
    }
    if let Some(mb) = args.soft_memory_mb {
        limits = limits.with_mem_high_mb(mb);
    }
    if let Some(w) = args.io_weight {
        limits.io_weight = Some(w);
    }
    if let Some(mb) = args.io_write_max_mb {
        limits = limits.with_io_write_max_mb(mb);
    }
    if let Some(mb) = args.io_read_max_mb {
        limits = limits.with_io_read_max_mb(mb);
    }
    if args.io_write_max_mb.is_some() || args.io_read_max_mb.is_some() {
        limits = limits.with_io_device(args.io_device.clone());
    }
    if let Some(c) = args.cpu_cores {
        limits.cpu_cores = Some(c);
    }
    if let Some(pct) = args.cpu_quota_pct {
        limits = limits.with_cpu_quota_pct(pct);
    }

    // Build the inner Command. `LogSink::Buffer` is the default in `Command::new`,
    // but for a passthrough wrapper we want the child's stdout/stderr to inherit
    // the parent's so callers see live output. The exec module wires sinks through
    // tokio piped IO; for true inheritance we'd need a separate path. For now,
    // use the default and let the wrapper print buffered output on completion if
    // the caller wants it (matches the bash `exec` semantics weakly — see report).
    let mut cmd = ExecCommand::new(args.argv[0].clone());
    if args.argv.len() > 1 {
        cmd = cmd.args(args.argv[1..].iter().cloned());
    }
    cmd = cmd.span("limits.exec");

    let limited = LimitedCommand::wrap(cmd, limits);
    let report = limited.run_unsupervised().await?;
    if let Some(buf) = &report.stdout_buf {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), buf);
    }
    if let Some(buf) = &report.stderr_buf {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), buf);
    }
    Ok(u8::try_from(report.exit_code.clamp(0, 255)).unwrap_or(1))
}

async fn limits_watchdog(args: LimitsWatchdogArgs) -> Result<u8, firestream_ci::Error> {
    // Foreground watchdog: poll the descendant tree at 1 Hz until the root
    // PID exits. Mirrors `bin/ci/memory-watchdog-darwin.sh`'s while-loop.
    // The Limits struct is the same shape as `limits exec`; we don't reuse
    // `LimitedCommand` because there's no child to spawn — the PID already
    // exists.
    let root_pid = args.pid;
    let soft = args.soft_mb;
    let hard = args.hard_mb;

    while pid_alive(root_pid) {
        let rss_mb = poll_tree_rss_mb(root_pid);
        if rss_mb >= hard {
            if let Some(victim) = largest_descendant(root_pid) {
                #[cfg(unix)]
                {
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(victim as i32),
                        nix::sys::signal::Signal::SIGTERM,
                    );
                }
                eprintln!("[firestream-ci limits watchdog] hard pressure rss_mb={rss_mb} victim={victim}");
            }
        } else if rss_mb >= soft {
            eprintln!("[firestream-ci limits watchdog] soft pressure rss_mb={rss_mb}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(0)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn poll_tree_rss_mb(root: u32) -> u64 {
    let output = match std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,rss="])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return 0,
    };
    let body = String::from_utf8_lossy(&output.stdout);
    let mut parent_of: std::collections::HashMap<u32, u32> = Default::default();
    let mut rss_of: std::collections::HashMap<u32, u64> = Default::default();
    for line in body.lines() {
        let mut it = line.split_ascii_whitespace();
        if let (Some(pid), Some(ppid), Some(rss)) = (it.next(), it.next(), it.next()) {
            if let (Ok(pid), Ok(ppid), Ok(rss)) =
                (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
            {
                parent_of.insert(pid, ppid);
                rss_of.insert(pid, rss);
            }
        }
    }
    let mut descendants: std::collections::HashSet<u32> = std::collections::HashSet::new();
    descendants.insert(root);
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for (child, parent) in &parent_of {
            if *parent == p && !descendants.contains(child) {
                descendants.insert(*child);
                frontier.push(*child);
            }
        }
    }
    let total_kb: u64 = descendants
        .iter()
        .map(|p| rss_of.get(p).copied().unwrap_or(0))
        .sum();
    total_kb / 1024
}

fn largest_descendant(root: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,rss="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut parent_of: std::collections::HashMap<u32, u32> = Default::default();
    let mut rss_of: std::collections::HashMap<u32, u64> = Default::default();
    for line in body.lines() {
        let mut it = line.split_ascii_whitespace();
        if let (Some(pid), Some(ppid), Some(rss)) = (it.next(), it.next(), it.next()) {
            if let (Ok(pid), Ok(ppid), Ok(rss)) =
                (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
            {
                parent_of.insert(pid, ppid);
                rss_of.insert(pid, rss);
            }
        }
    }
    let mut descendants: std::collections::HashSet<u32> = Default::default();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for (child, parent) in &parent_of {
            if *parent == p && !descendants.contains(child) && *child != root {
                descendants.insert(*child);
                frontier.push(*child);
            }
        }
    }
    let mut max: Option<(u32, u64)> = None;
    for d in &descendants {
        let rss = rss_of.get(d).copied().unwrap_or(0);
        match max {
            Some((_, best)) if rss <= best => {}
            _ => max = Some((*d, rss)),
        }
    }
    max.map(|(pid, _)| pid)
}

async fn oci_flatten(
    args: OciFlattenArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::oci::flatten::{LineageLabels, commit_flatten_builder};
    use firestream_ci::oci::shared_docker_client;

    let client = shared_docker_client()?;
    let labels = LineageLabels {
        branch: args.lineage_branch,
        sha: args.lineage_sha,
        epoch: args.lineage_epoch,
        parent_sha: args.lineage_parent_sha,
        trace_id: args.lineage_trace_id,
        arch: args.arch.clone(),
        flatten_ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
    };
    let known: Vec<&str> = args
        .known_arches
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // Flag wins; otherwise the profile's `builder.min_image_size_bytes`
    // (whose own schema default is `MIN_BUILDER_IMAGE_SIZE_BYTES`).
    let min = args
        .min_size_bytes
        .unwrap_or(profile.builder.min_image_size_bytes);

    let image_name = args
        .image_name
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| profile.builder.image_name.clone());
    if image_name.is_empty() {
        return Err(firestream_ci::Error::Other(
            "oci flatten: no image name — pass --image-name or set `builder.image_name` in the CI profile"
                .into(),
        ));
    }

    let result = commit_flatten_builder(
        &client,
        &args.container,
        &args.tag,
        args.src_image.as_deref(),
        labels,
        &image_name,
        &args.arch,
        &known,
        min,
        &profile.container_name_prefix(),
    )
    .await?;

    eprintln!(
        "firestream-ci oci flatten: tag={} size_bytes={} reaped={}",
        result.tag, result.size_bytes, result.reaped_count,
    );
    Ok(0)
}

fn report_summary(args: ReportSummaryArgs) -> Result<u8, firestream_ci::Error> {
    let dir = match args
        .rundir
        .map(|r| r.join("spans"))
        .or(args.span_dir)
        .or_else(|| std::env::var_os("OTEL_SPAN_DIR").map(PathBuf::from))
    {
        Some(d) => d,
        None => {
            eprintln!(
                "firestream-ci report summary: no --rundir / --span-dir and OTEL_SPAN_DIR is unset"
            );
            return Ok(0);
        }
    };
    let report = Report::from_span_dir(&dir)?;

    match args.format {
        ReportFormat::Markdown => {
            if !args.no_print {
                println!("{}", report.phase_table());
                println!();
                println!("{}", report.build_table());
            }
            if let Some(out) = args.output {
                if let Some(parent) = out.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                report.summary_markdown(&out)?;
                eprintln!("firestream-ci report summary: wrote {}", out.display());
            }
        }
        ReportFormat::Json => {
            // Pipe-friendly contract: when no `--output` is given, stdout
            // is a single valid JSON document and nothing else. Anything
            // diagnostic goes to stderr so `| jq` still works.
            match args.output {
                Some(out) => {
                    if let Some(parent) = out.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    report.write_json(&out)?;
                    eprintln!("firestream-ci report summary: wrote {}", out.display());
                }
                None => {
                    let value = report.to_json();
                    let body = serde_json::to_string_pretty(&value).map_err(|e| {
                        firestream_ci::Error::Other(format!("report summary: encoding JSON: {e}"))
                    })?;
                    println!("{body}");
                }
            }
        }
    }
    Ok(0)
}

// Reference unused imports so the binary keeps a stable feature set.
#[allow(dead_code)]
fn _link_tracer(t: Tracer) -> Tracer {
    t
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci sweep`
// ───────────────────────────────────────────────────────────────────────────

async fn sweep_cmd(args: SweepArgs) -> Result<u8, firestream_ci::Error> {
    if let Some(SweepSubcmd::Timer { cmd }) = args.cmd {
        return sweep_timer_cmd(cmd).await;
    }
    // No `--repo-root`: resolve the real repo root rather than trusting cwd,
    // so `sweep` from a subdir prunes the one `_build/` that exists.
    let root = match args.repo_root {
        Some(r) => r,
        None => {
            let cwd = std::env::current_dir()
                .map_err(|e| firestream_ci::Error::Other(format!("sweep: resolving cwd: {e}")))?;
            firestream_ci::rundir::find_repo_root(&cwd).unwrap_or(cwd)
        }
    };
    let roots = if args.all_worktrees {
        git_worktree_roots(&root).await
    } else {
        vec![root]
    };
    for r in &roots {
        // Sibling worktrees that have never built anything have nothing to
        // sweep; skip them so the multi-root pass stays quiet.
        if args.all_worktrees && !r.join("target").exists() && !r.join("_build").exists() {
            continue;
        }
        if roots.len() > 1 {
            eprintln!("firestream-ci sweep: worktree {}", r.display());
        }
        run_target_sweep(r, args.days, args.keep_rundirs, args.dry_run).await?;
        if let Some(gb) = args.max_target_gb {
            enforce_target_budget(r, args.days, gb, args.dry_run).await?;
        }
    }
    Ok(0)
}

/// All worktree roots of the repo containing `root`, per
/// `git worktree list --porcelain`. Falls back to just `root` (with a
/// warning) when git is unavailable or `root` isn't a repo — the sweep must
/// degrade to single-root, never fail outright.
async fn git_worktree_roots(root: &Path) -> Vec<PathBuf> {
    let out = tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .await;
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "firestream-ci sweep: git worktree list failed ({}); sweeping only {}",
                o.status,
                root.display()
            );
            return vec![root.to_path_buf()];
        }
        Err(e) => {
            eprintln!(
                "firestream-ci sweep: git not available ({e}); sweeping only {}",
                root.display()
            );
            return vec![root.to_path_buf()];
        }
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            let p = PathBuf::from(p);
            if !roots.contains(&p) {
                roots.push(p);
            }
        }
    }
    if roots.is_empty() {
        roots.push(root.to_path_buf());
    }
    roots
}

const GIB: u64 = 1 << 30;

/// Size backstop: when the age-based sweep leaves `target/` above budget,
/// escalate — tighter cargo-sweep windows first, then drop the
/// `incremental/` caches (pure cache: worst case is one warm rebuild of
/// recently edited crates, and historically the single largest category).
async fn enforce_target_budget(
    root: &Path,
    days: u64,
    budget_gb: u64,
    dry_run: bool,
) -> Result<(), firestream_ci::Error> {
    let target = root.join("target");
    if !target.exists() {
        return Ok(());
    }
    let budget = budget_gb * GIB;
    let mut size = dir_size(&target).await?;
    if size <= budget {
        eprintln!(
            "firestream-ci sweep: {} is {:.1} GiB (within {budget_gb} GiB budget)",
            target.display(),
            size as f64 / GIB as f64
        );
        return Ok(());
    }
    eprintln!(
        "firestream-ci sweep: {} is {:.1} GiB, over the {budget_gb} GiB budget — escalating",
        target.display(),
        size as f64 / GIB as f64
    );
    if dry_run {
        eprintln!(
            "firestream-ci sweep (dry-run): would re-sweep with tighter windows, then drop \
             target/*/incremental"
        );
        return Ok(());
    }

    let mut sweep_paths: Vec<PathBuf> = vec![root.to_path_buf()];
    if root.join("src/util/Cargo.toml").exists() {
        sweep_paths.push(root.join("src/util"));
    }
    let mut windows: Vec<u64> = vec![days / 2, days / 4, 1];
    windows.retain(|w| *w >= 1 && *w < days);
    windows.dedup();
    for w in windows {
        if size <= budget {
            break;
        }
        eprintln!("firestream-ci sweep: escalating to --time {w}");
        let mut cmd = tokio::process::Command::new("cargo-sweep");
        cmd.arg("sweep").args(["--time", &w.to_string()]);
        cmd.args(&sweep_paths);
        match cmd.status().await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("firestream-ci sweep: cargo-sweep not on PATH; skipping window escalation");
                break;
            }
            Err(e) => {
                return Err(firestream_ci::Error::Other(format!(
                    "sweep: spawning cargo-sweep: {e}"
                )));
            }
            Ok(st) if !st.success() => {
                eprintln!("firestream-ci sweep: cargo-sweep exited nonzero ({st}); continuing");
            }
            Ok(_) => {}
        }
        size = dir_size(&target).await?;
    }

    if size > budget {
        for inc in incremental_dirs(&target) {
            eprintln!("firestream-ci sweep: removing {}", inc.display());
            if let Err(e) = tokio::fs::remove_dir_all(&inc).await {
                eprintln!(
                    "firestream-ci sweep: removing {} failed (non-fatal): {e}",
                    inc.display()
                );
            }
        }
        size = dir_size(&target).await?;
    }
    eprintln!(
        "firestream-ci sweep: {} now {:.1} GiB{}",
        target.display(),
        size as f64 / GIB as f64,
        if size > budget {
            " (still over budget; next nightly window will keep tightening)"
        } else {
            ""
        }
    );
    Ok(())
}

async fn dir_size(path: &Path) -> Result<u64, firestream_ci::Error> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || dir_size_sync(&p))
        .await
        .map_err(|e| firestream_ci::Error::Other(format!("sweep: sizing task: {e}")))
}

fn dir_size_sync(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                if let Ok(md) = entry.metadata() {
                    total += md.len();
                }
            }
        }
    }
    total
}

/// `incremental/` cache dirs under a target root: `target/<profile>/` and,
/// for cross targets, `target/<triple>/<profile>/`.
fn incremental_dirs(target: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(rd) = std::fs::read_dir(target) else {
        return found;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let inc = p.join("incremental");
        if inc.is_dir() {
            found.push(inc);
        }
        if let Ok(rd2) = std::fs::read_dir(&p) {
            for e2 in rd2.flatten() {
                let inc2 = e2.path().join("incremental");
                if inc2.is_dir() {
                    found.push(inc2);
                }
            }
        }
    }
    found
}

/// Repo-local disk hygiene, shared by `firestream-ci sweep` and the CI tidy phase's
/// `target-sweep` task. Deletes only under the repo's own `target/` trees and
/// `_build/` rundirs — never `/nix/store` — which is why, unlike `nix gc`,
/// this carries no RUNNING_IN_DOCKER guard.
async fn run_target_sweep(
    root: &Path,
    days: u64,
    keep_rundirs: usize,
    dry_run: bool,
) -> Result<(), firestream_ci::Error> {
    // 1. cargo-sweep ages out target/ artifacts whose fingerprints haven't
    //    been used within the window. The binary ships in the devshell; when
    //    absent (builder container, bare host) this is a clean skip so the
    //    tidy phase never fails on it.
    let mut sweep_paths: Vec<PathBuf> = vec![root.to_path_buf()];
    // The isolated src/util workspace (firestream-ci itself) has its own target/
    // that `make clean` and root sweeps never reach.
    if root.join("src/util/Cargo.toml").exists() {
        sweep_paths.push(root.join("src/util"));
    }
    // Two complementary passes: `--time` ages out fingerprints unused within
    // the window; `--installed` drops artifacts orphaned by toolchain bumps
    // (a bump mints a whole new artifact universe and `--time` alone keeps
    // the old one warm for another window).
    for pass in [
        vec!["--time".to_string(), days.to_string()],
        vec!["--installed".to_string()],
    ] {
        let mut cmd = tokio::process::Command::new("cargo-sweep");
        cmd.arg("sweep").args(&pass);
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.args(&sweep_paths);
        match cmd.status().await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("firestream-ci sweep: cargo-sweep not on PATH; skipping target/ sweep");
                break;
            }
            Err(e) => {
                return Err(firestream_ci::Error::Other(format!(
                    "sweep: spawning cargo-sweep: {e}"
                )));
            }
            Ok(st) if !st.success() => {
                // Non-fatal: a partially swept target/ is still a valid target/.
                eprintln!("firestream-ci sweep: cargo-sweep exited nonzero ({st}); continuing");
            }
            Ok(_) => {}
        }
    }

    // 2. `_build/` rundir prune (newest `keep_rundirs` retained). RunDir::prune
    //    is grace-window-aware, so a concurrent CI run can't lose its rundir.
    let base = root.join("_build");
    if !base.exists() {
        return Ok(());
    }
    if dry_run {
        let total = firestream_ci::rundir::RunDir::count(&base).await.unwrap_or(0);
        eprintln!(
            "firestream-ci sweep (dry-run): {} rundirs under {}; would prune {} (keeping newest {})",
            total,
            base.display(),
            total.saturating_sub(keep_rundirs),
            keep_rundirs
        );
    } else {
        match firestream_ci::rundir::RunDir::prune(&base, keep_rundirs).await {
            Ok(n) => eprintln!(
                "firestream-ci sweep: pruned {n} rundirs from {} (kept newest {})",
                base.display(),
                keep_rundirs
            ),
            Err(e) => eprintln!("firestream-ci sweep: rundir prune failed (non-fatal): {e}"),
        }
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci sweep timer`
// ───────────────────────────────────────────────────────────────────────────

const SWEEP_UNIT_NAME: &str = "firestream-sweep";

async fn sweep_timer_cmd(cmd: SweepTimerCmd) -> Result<u8, firestream_ci::Error> {
    match cmd {
        SweepTimerCmd::Install(a) => sweep_timer_install(a).await,
        SweepTimerCmd::Uninstall => sweep_timer_uninstall().await,
        SweepTimerCmd::Status => sweep_timer_status().await,
    }
}

fn systemd_user_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config/systemd/user")
}

/// Directory on the current PATH containing `tool`, if any.
fn find_tool_dir(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find(|dir| dir.join(tool).is_file())
}

async fn systemctl_user(args: &[&str]) -> bool {
    tokio::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn sweep_timer_install(a: SweepTimerInstallArgs) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::service::{ScheduleKind, ServiceUnit};

    let repo_root = match a.repo_root {
        Some(r) => r,
        None => std::env::current_dir()
            .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: resolving cwd: {e}")))?,
    };
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);
    let exe = std::env::current_exe()
        .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: resolving own binary: {e}")))?;

    // The unit runs outside the devshell, so bake a PATH that reaches the
    // tools the sweep shells out to, as resolved right now. cargo/rustc must
    // be the devshell ones: cargo-sweep keys `--installed` off `rustc -vV`,
    // and a mismatched rustc marks every devshell artifact orphaned — a
    // nightly full-cache wipe. Without cargo-sweep the sweep silently
    // degrades to rundir pruning only. Refuse to install either way.
    let mut path_dirs: Vec<PathBuf> = Vec::new();
    for tool in ["cargo-sweep", "git", "cargo", "rustc"] {
        match find_tool_dir(tool) {
            Some(dir) => {
                if !path_dirs.contains(&dir) {
                    path_dirs.push(dir);
                }
            }
            None => {
                return Err(firestream_ci::Error::Other(format!(
                    "sweep timer install: `{tool}` not on PATH; run from inside the devshell"
                )));
            }
        }
    }
    for fallback in ["/usr/bin", "/bin"] {
        let p = PathBuf::from(fallback);
        if p.is_dir() && !path_dirs.contains(&p) {
            path_dirs.push(p);
        }
    }
    let baked_path = std::env::join_paths(&path_dirs)
        .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: composing PATH: {e}")))?
        .to_string_lossy()
        .into_owned();

    let unit = ServiceUnit::new(
        SWEEP_UNIT_NAME,
        "Firestream cargo target/ sweep (age + size budget, all worktrees)",
        &exe,
    )
    .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: {e}")))?
    .with_args([
        "sweep",
        "--all-worktrees",
        "--days",
        &a.days.to_string(),
        "--max-target-gb",
        &a.max_target_gb.to_string(),
        "--keep-rundirs",
        &a.keep_rundirs.to_string(),
        "--repo-root",
        &repo_root.to_string_lossy(),
    ])
    .with_env("PATH", baked_path)
    // systemd's OnCalendar spells nightly "daily"; the launchd cron mapper
    // only understands the "@daily" form.
    .with_schedule(ScheduleKind::Cron(
        if cfg!(target_os = "macos") {
            "@daily"
        } else {
            "daily"
        }
        .into(),
    ))
    // Deletes are metadata-heavy; idle IO class keeps the sweep from ever
    // contending with an active build.
    .with_service_extra("Nice=19")
    .with_service_extra("IOSchedulingClass=idle")
    .with_timer_extra("RandomizedDelaySec=30m");

    if cfg!(target_os = "macos") {
        let report = unit
            .install()
            .await
            .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: {e}")))?;
        eprintln!(
            "firestream-ci sweep timer: installed launchd agent ({} files)",
            report.files.len()
        );
        return Ok(0);
    }

    // Idempotent path: skip the systemctl churn when nothing changed, so
    // this is safe (and silent) on every devshell entry.
    let dir = systemd_user_dir();
    let desired_service = unit.render_systemd_service();
    let desired_timer = unit
        .render_systemd_timer()
        .expect("Cron schedule always renders a timer");
    let service_path = dir.join(format!("{SWEEP_UNIT_NAME}.service"));
    let timer_path = dir.join(format!("{SWEEP_UNIT_NAME}.timer"));
    let unchanged = std::fs::read_to_string(&service_path).is_ok_and(|s| s == desired_service)
        && std::fs::read_to_string(&timer_path).is_ok_and(|s| s == desired_timer);
    let enabled =
        systemctl_user(&["is-enabled", "--quiet", &format!("{SWEEP_UNIT_NAME}.timer")]).await;
    if unchanged && enabled {
        eprintln!("firestream-ci sweep timer: already installed and up to date");
        return Ok(0);
    }

    let report = unit
        .install_systemd_user()
        .await
        .map_err(|e| firestream_ci::Error::Other(format!("sweep timer: {e}")))?;
    // The unit may already be loaded with the old definition; reload and
    // restart the timer so the new schedule/ExecStart take effect now.
    systemctl_user(&["daemon-reload"]).await;
    systemctl_user(&["restart", &format!("{SWEEP_UNIT_NAME}.timer")]).await;
    match &report.backend_error {
        Some(e) => eprintln!("firestream-ci sweep timer: unit files written but systemctl failed: {e}"),
        None => eprintln!(
            "firestream-ci sweep timer: installed (nightly, days={}, max-target-gb={}, root={})",
            a.days,
            a.max_target_gb,
            repo_root.display()
        ),
    }
    Ok(0)
}

async fn sweep_timer_uninstall() -> Result<u8, firestream_ci::Error> {
    if cfg!(target_os = "macos") {
        let plist = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(format!("Library/LaunchAgents/{SWEEP_UNIT_NAME}.plist"));
        let _ = tokio::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist)
            .status()
            .await;
        let _ = std::fs::remove_file(&plist);
        eprintln!("firestream-ci sweep timer: uninstalled");
        return Ok(0);
    }
    systemctl_user(&["disable", "--now", &format!("{SWEEP_UNIT_NAME}.timer")]).await;
    let dir = systemd_user_dir();
    let _ = std::fs::remove_file(dir.join(format!("{SWEEP_UNIT_NAME}.service")));
    let _ = std::fs::remove_file(dir.join(format!("{SWEEP_UNIT_NAME}.timer")));
    systemctl_user(&["daemon-reload"]).await;
    eprintln!("firestream-ci sweep timer: uninstalled");
    Ok(0)
}

async fn sweep_timer_status() -> Result<u8, firestream_ci::Error> {
    if cfg!(target_os = "macos") {
        let _ = tokio::process::Command::new("launchctl")
            .args(["list", SWEEP_UNIT_NAME])
            .status()
            .await;
        return Ok(0);
    }
    let timer = format!("{SWEEP_UNIT_NAME}.timer");
    if !systemctl_user(&["list-timers", "--all", "--no-pager", &timer]).await {
        eprintln!("firestream-ci sweep timer: not installed (or systemd unavailable)");
        return Ok(0);
    }
    let _ = tokio::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            &format!("{SWEEP_UNIT_NAME}.service"),
            "-n",
            "20",
            "--no-pager",
        ])
        .status()
        .await;
    Ok(0)
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci nix gc`
// ───────────────────────────────────────────────────────────────────────────

async fn nix_gc(args: NixGcArgs) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::nix::gc;

    let in_docker = std::env::var("RUNNING_IN_DOCKER").as_deref() == Ok("1");
    if !in_docker && !args.allow_host {
        eprintln!(
            "firestream-ci nix gc: skipped (RUNNING_IN_DOCKER unset; pass --allow-host to override — \
             the host /nix/store will be touched)"
        );
        return Ok(0);
    }

    // Resolve nix system: explicit flag → NIX_SYSTEM env → `nix eval`.
    let system = match args.system {
        Some(s) => s,
        None => match std::env::var("NIX_SYSTEM") {
            Ok(s) if !s.is_empty() => s,
            _ => detect_nix_system()?,
        },
    };

    // Rundir pruning (newest `keep_rundirs` retained). Runs before the
    // store GC so the to-be-pruned roots no longer hold refs to nix paths
    // we're about to collect. `RunDir::prune` itself is grace-window-aware,
    // so a concurrent run can't have its sibling reaped mid-build.
    let base = firestream_ci::rundir::default_build_root();
    match firestream_ci::rundir::RunDir::prune(&base, args.keep_rundirs).await {
        Ok(n) => eprintln!(
            "firestream-ci nix gc: pruned {n} rundirs from {} (kept newest {})",
            base.display(),
            args.keep_rundirs
        ),
        Err(e) => eprintln!("firestream-ci nix gc: rundir prune failed (non-fatal): {e}"),
    }

    // One-time legacy-roots scrub. The spans/logs/watchdog roots beneath
    // `_build/` are dead once the rundir contract owns them; opportunistic
    // rm-rf on first invocation post-merge, then no-op forever after.
    if !args.no_legacy_cleanup {
        for legacy in ["spans", "logs", "watchdog"] {
            let p = base.join(legacy);
            if p.exists() {
                match std::fs::remove_dir_all(&p) {
                    Ok(_) => eprintln!("firestream-ci nix gc: removed legacy root {}", p.display()),
                    Err(e) => eprintln!(
                        "firestream-ci nix gc: legacy cleanup of {} failed (non-fatal): {e}",
                        p.display()
                    ),
                }
            }
        }
    }

    // Standalone CLI: no dashboard, no log routing — preserve the prior
    // inherit-stderr behavior so users see nix output directly.
    let report = gc(
        &args.flake_path,
        &system,
        &args.roots_dir,
        args.max_jobs,
        args.cores,
        None,
        None,
    )?;
    eprintln!(
        "firestream-ci nix gc: roots_ok={} roots_failed={} roots_total={} deleted_paths={}",
        report.roots_ok,
        report.roots_failed,
        report.roots_total,
        report.deleted_paths.len(),
    );
    Ok(0)
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci platform`
// ───────────────────────────────────────────────────────────────────────────

fn platform_decide(
    args: PlatformDecideArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::platform::{self, DecisionSource};

    let target = args.target.unwrap_or_else(detect_host_arch);
    let d = platform::decide_with_profile(Some(&target), profile);

    if args.json {
        let doc = serde_json::json!({
            "target": platform::norm_arch(&target),
            "strategy": d.strategy.label(),
            "blocker": d.blocker,
            "source": match d.source {
                DecisionSource::EnvOverride => "env-override",
                DecisionSource::ProfileDefault => "profile-default",
                DecisionSource::Probed => "probed",
            },
            "warnings": d.warnings,
            // RAW target, not norm_arch: see platform::docker_arch_alias.
            "nix_store_volume": platform::docker_cache_volume(
                &profile.build_strategy.docker_cache_volume,
                &target,
            ),
            "native_cache": profile.build_strategy.native_cache,
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return Ok(0);
    }

    // stdout: the answer, alone, so `$(...)` captures exactly the word.
    println!("{}", d.strategy.label());

    // stderr: the reasoning.
    let source = match d.source {
        DecisionSource::EnvOverride => {
            format!("forced by {}", platform::ENV_BUILD_STRATEGY)
        }
        DecisionSource::ProfileDefault => format!(
            "forced by the CI profile (build_strategy.default={})",
            profile.build_strategy.default
        ),
        DecisionSource::Probed => "probed".to_string(),
    };
    eprintln!("target     {}", platform::norm_arch(&target));
    eprintln!("source     {source}");
    match &d.blocker {
        Some(b) => eprintln!("blocker    {b}"),
        None => eprintln!("blocker    (none — native is available)"),
    }
    for w in &d.warnings {
        eprintln!("warning    {w}");
    }
    Ok(0)
}

fn platform_probe() -> Result<u8, firestream_ci::Error> {
    let probe = firestream_ci::platform::Probe::detect_with_docker_daemon();
    println!(
        "{}",
        serde_json::to_string_pretty(&probe).map_err(|e| firestream_ci::Error::Other(
            format!("platform probe: serialising: {e}")
        ))?
    );
    Ok(0)
}

fn detect_nix_system() -> Result<String, firestream_ci::Error> {
    let out = std::process::Command::new("nix")
        .args([
            "eval",
            "--impure",
            "--raw",
            "--expr",
            "builtins.currentSystem",
        ])
        .output()
        .map_err(|e| firestream_ci::Error::Other(format!("nix eval currentSystem: {e}")))?;
    if !out.status.success() {
        return Err(firestream_ci::Error::Other(format!(
            "nix eval currentSystem failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(firestream_ci::Error::Other(
            "nix eval currentSystem returned empty".into(),
        ));
    }
    Ok(s)
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci k8s namespace`
// ───────────────────────────────────────────────────────────────────────────

fn k8s_namespace(profile: &firestream_ci::Profile) -> Result<u8, firestream_ci::Error> {
    let branch = resolve_branch_for_k8s()?;
    let safe: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(53)
        .collect();
    // Prefix is profile data (`project.k8s_namespace_prefix`, defaulting to
    // `"<project.name>-"`). Branch sanitisation above is generic — the DNS-1123
    // label rules it enforces belong to Kubernetes, not to any project.
    print!("{}", profile.project.k8s_namespace_prefix());
    println!("{safe}");
    Ok(0)
}

fn resolve_branch_for_k8s() -> Result<String, firestream_ci::Error> {
    if let Ok(b) = std::env::var("BRANCH") {
        if !b.is_empty() {
            return Ok(b);
        }
    }
    if let Ok(b) = std::env::var("BRANCH_NAME") {
        if !b.is_empty() {
            return Ok(b.replace('/', "-"));
        }
    }
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| firestream_ci::Error::Other(format!("git rev-parse: {e}")))?;
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || raw.is_empty() || raw == "HEAD" {
        return Err(firestream_ci::Error::Other(
            "detached HEAD — cannot derive branch name. Check out a branch first.".into(),
        ));
    }
    Ok(raw.replace('/', "-"))
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci ci-linux`
// ───────────────────────────────────────────────────────────────────────────
//
// THE PHASE SET COMES FROM THE PROFILE. There is no phase list, no phase
// name, and no phase dependency edge in this file: `run_phases` walks
// `Profile::phase_order(mode)` and materialises each phase's tasks from that
// phase's own declaration — `builtin_tasks` (from the runner's closed
// vocabulary), `shell_tasks` (free-form argv), and `attrs` (nix attributes,
// aggregated into one `FastBuild` when the phase asks for it).
//
// What that buys, concretely: Firestream's `tidy -> verify -> build -> attest`
// shape lives in `bin/nix/firestream/ci/profile.nix`, a project with a
// different shape needs no Rust change, and the phase DAG printed by
// `--dry-run` is by construction the same DAG that executes.
//
// Fail-fast: a phase whose `depends_on` names a FAILED Required phase is
// skipped (`Pipeline::run`). The bash original ran everything unconditionally
// for span coverage; we trade that for fast feedback.

#[derive(Clone, Debug)]
struct CiLinuxCtx {
    mode: String,
    nix_system: String,
    arch: String,
    in_docker: bool,
    rundir: RunDir,
    skip_gc: bool,
    /// Phase 4 — artifact materialisation policy. Threaded through to
    /// `build_nix_attr_task` so the per-attr success branch can call
    /// `export_build_artifact` without reading env vars.
    artifacts_format: ArtifactsFormat,
    artifacts_max_bytes: u64,
    /// Phase 6 — opt-out for the auto-replay to Honeycomb at end-of-run.
    /// Default `false` (auto-replay fires when `HONEYCOMB_API_KEY` is set).
    no_export_honeycomb: bool,
    /// Phase 6 — force replay even on `Outcome::Failed`. Default `false`
    /// (only `Outcome::Passed` triggers the auto-ship).
    export_honeycomb_on_failure: bool,
    /// Live-dashboard handle, `Some` only when superconsole is the active
    /// UI. Task builders read this to wire a `ring_sink` into FastBuild
    /// so each task's stderr feeds the dashboard's tail-N preview.
    dashboard: Option<std::sync::Arc<firestream_ci::dashboard::DashboardReporter>>,
    /// Phase 4 — the loaded CI profile (`ci-manifest.json`). Every
    /// project-specific value the run needs (phase attrs, tier rules, export
    /// targets, devshell sentinels, builder identity) comes from here.
    /// `Arc` because the ctx is cloned once per phase closure.
    profile: std::sync::Arc<firestream_ci::Profile>,
    /// The resolved native-vs-docker decision for this run
    /// (`firestream_ci::platform`, the single authority). Read once at ctx
    /// construction so every consumer — the header line, the tidy phase's
    /// host-store guard, the manifest — sees the same answer.
    strategy: firestream_ci::platform::Decision,
    /// Per-`nix build` cgroup budget for the parallel phases. `None` when
    /// limits are disabled (`FIRESTREAM_CI_LIMITS=0`).
    budget: Option<firestream_ci::limits::BuildBudget>,
    /// Absolute io.max write cap in MB/s applied alongside `MemoryMax`.
    /// `None`/0 leaves IO unthrottled.
    io_write_max_mb: Option<u64>,
    /// Run-scoped trace root. Every task opens a child span under it, and the
    /// `json+file` sink writes each span to `<rundir>/spans/` — no collector
    /// in the loop, which is what makes a laptop run observable.
    span_root: Option<std::sync::Arc<firestream_ci::trace::Span>>,
    /// Owner of the OTLP transport. Held so `shutdown()` can drain the
    /// buffered network leg once, at end of run; the disk leg is synchronous
    /// on span drop and needs nothing from here.
    tracer: Option<firestream_ci::trace::Tracer>,
}

impl RunContext for CiLinuxCtx {
    fn rundir(&self) -> &RunDir {
        &self.rundir
    }
}

impl CiLinuxCtx {
    /// Template-expansion context. The CLI's `--nix-system` / `--arch` (and
    /// their env fallbacks) win over the values baked into the profile, so a
    /// single manifest can still be driven at an explicit system.
    fn expand_ctx(&self) -> firestream_ci::profile::ExpandCtx {
        firestream_ci::profile::ExpandCtx {
            system: self.nix_system.clone(),
            arch: self.arch.clone(),
            project: self.profile.project.name.clone(),
        }
    }

    /// A child span of the run root, named `<phase>:<task>`. `None` when
    /// tracing failed to initialise. The returned span ends on drop, so a task
    /// simply binds it (`let _sp = …`) for its whole body and the duration is
    /// the task's real wall clock.
    fn child_span(&self, phase: &str, task: &str) -> Option<firestream_ci::trace::Span> {
        self.span_root.as_ref().map(|root| {
            let mut s = root.child(format!("{phase}:{task}"));
            // `phase` is what `report summary` groups its phase table by
            // (see `report::span_to_entry`); without it every CI span lands
            // in neither table and the summary renders two empty grids.
            s.set_attribute("phase", phase.to_string());
            s.set_attribute("task", task.to_string());
            s
        })
    }
}

async fn ci_linux(
    args: CiLinuxArgs,
    profile: std::sync::Arc<firestream_ci::Profile>,
) -> Result<u8, firestream_ci::Error> {
    // "Agent mode": stdout becomes a pure stream of typed `firestream.ci.v1.Event`
    // frames. All human progress (banner, dashboard, footer) already goes to
    // stderr, so stdout is free for the protocol.
    let json_mode = matches!(args.output_format, OutputFormat::Json);
    let dry_run = args.dry_run;
    let ctx = build_ci_linux_ctx(args, profile).await?;

    // `--dry-run`: print the phase DAG + tiers + resolved attrs and exit.
    // Runs BEFORE the devshell guard on purpose — inspecting a profile is
    // not a build and must work from a plain shell.
    if dry_run {
        print_profile_dry_run(&ctx.profile, &ctx.mode, &ctx.expand_ctx());
        // Still assert the invariant, so `--dry-run` is a real gate on a
        // profile that would be a no-op rather than a pretty printer for one.
        assert_runnable_phases(&ctx)?;
        return Ok(0);
    }

    // A CI run that executes ZERO phases and exits 0 is the worst possible
    // failure mode: it is indistinguishable from a green build. Three ways to
    // arrive there, all now fatal:
    //   * no profile resolved at all (handled in `dispatch`, which uses the
    //     strict resolver for this subcommand — a missing/misspelled
    //     `--profile` is an error, not a fallback to the empty default);
    //   * a profile whose phases are all filtered out by `--mode`;
    //   * a profile emitted for a system where every phase is empty — which is
    //     exactly Firestream's Darwin manifest, and is how a Darwin
    //     `ci-linux` becomes an explicit "not supported here".
    assert_runnable_phases(&ctx)?;

    // Prereq guard — mirror bash lines 26-32. Inside docker the base image
    // provides the toolchain directly; on a host runner the Nix devshell is
    // required so nix-fast-build, jq, otel-cli, etc. are guaranteed present.
    // Which env vars prove "devshell" is profile data
    // (`devshell_sentinels`), not a literal.
    if !ctx.in_docker && !ctx.profile.in_devshell(&|k: &str| std::env::var(k).ok()) {
        eprintln!(
            "firestream-ci ci-linux: must run inside the Nix devshell. Enter it first:\n  \
             nix develop --command bash -c 'CI_RUNNER=host-linux make ci'\n\
             (devshell sentinels from the CI profile: {})",
            ctx.profile.devshell_sentinel_summary()
        );
        return Ok(2);
    }

    // `RunDir::create` (in build_ci_linux_ctx) already mkdir'd the per-run
    // logs/spans/profiles/artifacts subdirs and wrote .sentinel. No
    // additional create_dir_all needed here.

    // CI is non-incremental by contract: fresh workspaces gain nothing from
    // incremental caches, and incremental units bypass sccache. The devshell
    // deliberately does NOT export CARGO_INCREMENTAL anymore ([profile.dev]
    // incremental=true governs there), so the CI entrypoint asserts it for
    // every cargo-touching child. `:=`-style — an explicit override wins.
    set_default_env("CARGO_INCREMENTAL", "0");

    print_ci_manifest(&ctx);

    // Agent-mode: announce the rundir up front so a caller learns where the
    // full logs will land before the (long) pipeline runs.
    if json_mode {
        let ev = firestream_ci::wire::Event::run_started(ctx.rundir.path(), &ctx.mode, &ctx.nix_system);
        let _ = firestream_ci::wire::write_frame(&mut std::io::stdout(), &ev);
    }

    // Install a global SIGINT/SIGTERM trap that (a) flips the shared abort
    // flag and (b) wakes a Notify so the running pipeline can observe the
    // cancel via tokio::select. A second signal force-exits via the
    // handler itself. The scopeguard below picks the flag up on unwind and
    // writes a manifest with outcome=aborted so partial runs are still
    // discoverable.
    let (abort, cancel) = install_signal_handler();

    // Scopeguard finalizer: if the success path doesn't run (panic, early
    // ?-return, signal abort), write a manifest with outcome=aborted so the
    // rundir is still discoverable. `finalize_done` is flipped on the
    // success/failure paths so the guard doesn't double-write.
    let finalize_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = {
        // Clone WITHOUT the trace handles. The guard only needs the rundir,
        // and an `Arc<Span>` clone parked here until end-of-scope would keep
        // the run's root span open past `finalize_manifest` — which is where
        // `profiles/spans.ndjson` is aggregated, so the root would be missing
        // from it. (Found exactly that way: 5 span.json files on disk, 4 rows
        // in the ndjson.)
        let mut ctx = ctx.clone();
        ctx.span_root = None;
        ctx.tracer = None;
        let finalize_done = finalize_done.clone();
        let abort = abort.clone();
        scopeguard::guard((), move |_| {
            if finalize_done.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            // The async finalizer needs a runtime to run on. We're inside
            // `#[tokio::main]`; spawn_blocking can't reach the reactor from
            // a drop. Use a fresh single-thread runtime — cheap, and the
            // finalize is a single file write.
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok();
                if let Some(rt) = rt {
                    rt.block_on(async {
                        let _ = finalize_manifest(&ctx, Outcome::Aborted).await;
                    });
                }
            })
            .join();
            let _ = abort; // keep the AtomicBool alive across the guard.
        })
    };

    let mut ctx = ctx;
    let overall_exit = run_phases(&ctx, cancel).await?;

    // Close the run's root span BEFORE finalizing: `finalize_manifest`
    // aggregates `<rundir>/spans/` into `profiles/spans.ndjson`, and a root
    // still held open at that point would be missing from the aggregate even
    // though its `span.json` lands moments later. Every task span is already
    // dropped (the pipeline has returned), so `try_unwrap` succeeds; if a
    // stray clone somehow survives we simply let the Arc drop normally.
    if let Some(root) = ctx.span_root.take() {
        match std::sync::Arc::try_unwrap(root) {
            Ok(span) => span.finish(),
            Err(arc) => drop(arc),
        }
    }
    // Drain the network leg (no-op offline — the disk sink already wrote).
    if let Some(t) = &ctx.tracer {
        if let Err(e) = t.shutdown().await {
            eprintln!("firestream-ci ci-linux: span export failed (non-fatal): {e}");
        }
    }
    let ctx = ctx;

    let outcome = if overall_exit == 0 {
        Outcome::Passed
    } else {
        Outcome::Failed
    };

    // Finalise manifest (collate streaming entries). Idempotent — other runners
    // can merge its additions on top later on the same host.
    finalize_manifest(&ctx, outcome).await?;
    finalize_done.store(true, std::sync::atomic::Ordering::SeqCst);

    // Phase 6 — auto-ship spans to Honeycomb when configured. No-op offline
    // (no API key) and on aborted runs. See `maybe_export_honeycomb`.
    maybe_export_honeycomb(
        &ctx.rundir,
        outcome,
        ctx.no_export_honeycomb,
        ctx.export_honeycomb_on_failure,
    )
    .await;

    // End-of-run summary. Mirrors `ci-tail.sh::ci_summary_emit`.
    let _ = write_summary(&ctx);

    // Single trailing artifacts block — replaces the three scattered
    // inline path emissions (`attest:`, `manifest:`, `CI summary:`).
    print_artifacts_footer(&ctx.rundir);

    let exit = overall_exit;

    // Agent-mode terminal frame: the structured verdict + the full logs dir +
    // the failing attrs (each with a concrete path to dig into).
    if json_mode {
        let outcome_str = match outcome {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Aborted => "aborted",
        };
        let failed = collect_failed_attrs(&ctx.rundir);
        let ev = firestream_ci::wire::Event::run_finished(
            outcome_str,
            i32::from(exit),
            ctx.rundir.path(),
            ctx.rundir.logs_dir(),
            failed,
        );
        let _ = firestream_ci::wire::write_frame(&mut std::io::stdout(), &ev);
    }

    if exit != 0 {
        eprintln!(
            "firestream-ci ci-linux: pipeline failed ({} mode, exit={exit}).",
            ctx.mode
        );
    }
    Ok(exit)
}

/// Fail unless the resolved profile yields at least one phase with actual work
/// in this mode. See the call site for why a zero-phase run must never be
/// green.
fn assert_runnable_phases(ctx: &CiLinuxCtx) -> Result<(), firestream_ci::Error> {
    if !ctx.profile.runnable_phases(&ctx.mode).is_empty() {
        return Ok(());
    }
    let declared: Vec<&str> = ctx.profile.phases.iter().map(|p| p.name.as_str()).collect();
    Err(firestream_ci::Error::Other(format!(
        "ci-linux: the resolved CI profile declares no runnable phases for \
         system={} mode={}.\n  \
         declared phases: {}\n  \
         profile: {}\n  \
         A run with no phases would exit 0 and be indistinguishable from a \
         green build, so this is fatal. Either the profile is for a different \
         system (Firestream's Darwin manifest is deliberately empty — \
         `ci-linux` is the Linux runner; use `ci` to dispatch), or --mode={} \
         filters every phase out.",
        ctx.nix_system,
        ctx.mode,
        if declared.is_empty() { "(none)".to_string() } else { declared.join(", ") },
        std::env::var(firestream_ci::profile::PROFILE_ENV).unwrap_or_else(|_| "(unresolved)".into()),
        ctx.mode,
    )))
}

/// Scan `<rundir>/profiles/<phase>.json` (written by `write_phase_summaries`)
/// for attrs that failed, mapping each to a concrete path a caller can explore.
/// Best-effort: a missing/garbled profiles dir yields an empty list.
fn collect_failed_attrs(rundir: &RunDir) -> Vec<firestream_ci::wire::FailedAttr> {
    let mut out = Vec::new();
    let dir = rundir.profiles_dir();
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let phase = doc
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(attrs) = doc.get("attrs").and_then(|v| v.as_array()) else {
            continue;
        };
        for a in attrs {
            let failed = a.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
            if failed == 0 {
                continue;
            }
            out.push(firestream_ci::wire::FailedAttr {
                phase: phase.clone(),
                attr: a
                    .get("attr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                detail_path: a
                    .get("result_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    out
}

/// Install a SIGINT (and SIGTERM on Unix) handler that drives a two-stage
/// shutdown: first signal flips a shared `AtomicBool` AND wakes any
/// `Notify` waiters (so `tokio::select!` arms can cancel cleanly); any
/// subsequent signal calls `std::process::exit(130)` immediately. The
/// async fn is not signal-safe — we still avoid calling `finalize_manifest`
/// from the handler — but the second-^C escalation guarantees `firestream-ci`
/// always exits when the user asks it to, even if the graceful path is
/// wedged on a stuck subprocess (e.g. an hours-long `nix-collect-garbage`).
fn install_signal_handler() -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<tokio::sync::Notify>,
) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let f = flag.clone();
    let n = notify.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return,
            };
            // First signal: graceful shutdown.
            tokio::select! {
                _ = sigint.recv() => {},
                _ = sigterm.recv() => {},
            }
            f.store(true, std::sync::atomic::Ordering::SeqCst);
            n.notify_waiters();
            eprintln!("firestream-ci: caught signal, shutting down (press ^C again to force exit)");
            // Any subsequent signal (SIGINT or SIGTERM) forces exit.
            tokio::select! {
                _ = sigint.recv() => {},
                _ = sigterm.recv() => {},
            }
            eprintln!("firestream-ci: force exit");
            std::process::exit(130);
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            f.store(true, std::sync::atomic::Ordering::SeqCst);
            n.notify_waiters();
            eprintln!("firestream-ci: caught signal, shutting down (press ^C again to force exit)");
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("firestream-ci: force exit");
            std::process::exit(130);
        }
    });
    (flag, notify)
}

async fn build_ci_linux_ctx(
    args: CiLinuxArgs,
    profile: std::sync::Arc<firestream_ci::Profile>,
) -> Result<CiLinuxCtx, firestream_ci::Error> {
    let mode = args
        .mode
        .or_else(|| std::env::var("CI_MODE").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".into());

    // Precedence: --nix-system → NIX_SYSTEM → the profile's baked
    // `project.nix_system`. The profile entry means a manifest built by
    // `nix build .#firestream-ci-profile` already knows its own system, so a
    // bare `firestream-ci ci-linux --dry-run` works without a Makefile export.
    let nix_system = match args.nix_system {
        Some(s) => s,
        None => std::env::var("NIX_SYSTEM")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(profile.project.nix_system.clone()).filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                firestream_ci::Error::Other(
                    "NIX_SYSTEM must be set (Makefile-exported, --nix-system, \
                     or `project.nix_system` in the CI profile)"
                        .into(),
                )
            })?,
    };

    let arch = args
        .arch
        .or_else(|| std::env::var("HOST_ARCH").ok())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(profile.project.arch.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(detect_host_arch);

    let in_docker = std::env::var("RUNNING_IN_DOCKER").as_deref() == Ok("1");

    // Run-dir resolution via the `RunDir` contract. Explicit-flag/env path
    // (`BUILD_OUTPUT_DIR`) means the container is adopting a host-allocated
    // rundir; call `RunDir::open` + `verify_sentinel` so a mis-mounted
    // `docker -v` is caught loudly. Otherwise allocate fresh via
    // `RunDir::create`, which writes the sentinel, builds the subdir tree,
    // and returns a typed handle.
    let rundir = match args.build_output_dir {
        Some(p) => {
            let r = RunDir::open(&p)
                .await
                .map_err(|e| firestream_ci::Error::Other(format!("rundir open {}: {e}", p.display())))?;
            if in_docker {
                r.verify_sentinel().await.map_err(|e| {
                    firestream_ci::Error::Other(format!(
                        "rundir sentinel verification failed for {}: {e} \
                         (mis-mounted docker volume?)",
                        p.display()
                    ))
                })?;
            }
            r
        }
        None => match std::env::var("BUILD_OUTPUT_DIR") {
            Ok(s) if !s.is_empty() && in_docker => {
                let p = PathBuf::from(s);
                let r = RunDir::open(&p).await.map_err(|e| {
                    firestream_ci::Error::Other(format!("rundir open {}: {e}", p.display()))
                })?;
                r.verify_sentinel().await.map_err(|e| {
                    firestream_ci::Error::Other(format!(
                        "rundir sentinel verification failed for {}: {e} \
                         (mis-mounted docker volume?)",
                        p.display()
                    ))
                })?;
                r
            }
            _ => allocate_run_dir().await?,
        },
    };

    // Spans land in `<rundir>/spans/` unconditionally. Phase 2 made the
    // rundir authoritative for span output — the legacy `--otel-span-dir`
    // CLI flag and `OTEL_SPAN_DIR` env var are still accepted at the CLI
    // surface for back-compat (callers constructing a bare `FastBuild` via
    // the bash bridge can still rely on the env-var fallback inside
    // `FastBuild::run`), but the binary's own CI dispatch ignores them: the
    // per-task `FastBuild` invocations are constructed with
    // `.spans_dir(ctx.rundir.spans_dir())` so the builder field wins.
    let _ = args.otel_span_dir;

    // Part C: one authority for native-vs-docker. `decide_with_profile` is the
    // call site that finally reads `build_strategy.default` from the profile
    // (`FIRESTREAM_BUILD_STRATEGY` still wins, before any probing).
    let strategy = firestream_ci::platform::decide_with_profile(Some(&arch), &profile);

    // Cgroup budget for the parallel build/verify phases. See the plan's risk
    // row: "Parallel FastBuild on the documented devcontainer minimum (4 CPU /
    // 8 GB) will OOM. `limits` must land in the SAME phase as the parallel
    // build." Both numbers — the per-child MemoryMax and the concurrency —
    // come out of one RAM pool so the aggregate is actually bounded.
    let limits_on = std::env::var("FIRESTREAM_CI_LIMITS").as_deref() != Ok("0");
    let requested_jobs = env_usize("NIX_BUILD_MAX_JOBS").unwrap_or(DEFAULT_MAX_JOBS);
    let budget = limits_on.then(|| firestream_ci::limits::BuildBudget::detect(requested_jobs));
    let io_write_max_mb = std::env::var("FIRESTREAM_CI_IO_WRITE_MAX_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IO_WRITE_MAX_MB);

    // Trace root. `checkpoint_dir` IS the `json+file` sink: every span is
    // written to `<rundir>/spans/<trace>/<span>/span.json` on drop, with no
    // OTLP collector required. Network export stays opt-in via
    // OTEL_EXPORTER_OTLP_ENDPOINT / the Honeycomb replay at end of run.
    let (span_root, tracer) = match firestream_ci::trace::Tracer::builder()
        .service("firestream-ci")
        .checkpoint_dir(rundir.spans_dir())
        .build()
        .await
    {
        Ok(tracer) => {
            let mut root = tracer.root_span("ci-linux");
            root.set_attribute("ci.mode", mode.clone());
            root.set_attribute("ci.nix_system", nix_system.clone());
            root.set_attribute("ci.arch", arch.clone());
            root.set_attribute("ci.strategy", strategy.strategy.label().to_string());
            (Some(std::sync::Arc::new(root)), Some(tracer))
        }
        Err(e) => {
            eprintln!("firestream-ci ci-linux: tracing disabled (non-fatal): {e}");
            (None, None)
        }
    };

    Ok(CiLinuxCtx {
        mode,
        nix_system,
        arch,
        in_docker,
        rundir,
        skip_gc: args.skip_gc,
        artifacts_format: args.artifacts_format,
        artifacts_max_bytes: args.artifacts_max_bytes,
        no_export_honeycomb: args.no_export_honeycomb,
        export_honeycomb_on_failure: args.export_honeycomb_on_failure,
        // Populated by run_phases when select_reporter picks the dashboard.
        dashboard: None,
        profile,
        strategy,
        budget,
        io_write_max_mb: (io_write_max_mb > 0).then_some(io_write_max_mb),
        span_root,
        tracer,
    })
}

/// `nix-fast-build` concurrency before the memory budget has its say. 4 is
/// enough to overlap independent derivations without stampeding the store
/// lock (`cores=0` already gives each build every core).
const DEFAULT_MAX_JOBS: usize = 4;

/// Default absolute write-bandwidth cap per nix build child, in MB/s
/// (systemd `IOWriteBandwidthMax=`, cgroup io.max). Chosen high enough not to
/// slow a real build and low enough that a runaway one cannot wedge the host's
/// interactive responsiveness. `FIRESTREAM_CI_IO_WRITE_MAX_MB=0` disables.
const DEFAULT_IO_WRITE_MAX_MB: u64 = 512;

/// Allocate a fresh run dir via the canonical `RunDir::create` contract.
/// Replaces the hand-rolled path construction that used to bypass the
/// sentinel + subdir creation.
async fn allocate_run_dir() -> Result<RunDir, firestream_ci::Error> {
    // Repo-root-anchored, NOT cwd-relative — see `rundir::default_build_root`.
    let base = firestream_ci::rundir::default_build_root();
    let sha = git_short_sha().unwrap_or_else(|| "nogit".into());
    RunDir::create(&base, &sha)
        .await
        .map_err(|e| firestream_ci::Error::Other(format!("allocate_run_dir: {e}")))
}

fn git_short_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn detect_host_arch() -> String {
    let out = std::process::Command::new("uname").arg("-m").output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => std::env::consts::ARCH.to_string(),
    }
}

/// `--dry-run` output: the phase DAG, each phase's tier, and the attrs the
/// profile resolves to at this system/arch. Prints to **stdout** so it can be
/// piped/diffed; nothing is executed.
fn print_profile_dry_run(
    p: &firestream_ci::Profile,
    mode: &str,
    ectx: &firestream_ci::profile::ExpandCtx,
) {
    println!("profile        schema_version={}", p.schema_version);
    println!(
        "project        name={} nix_system={} arch={}",
        p.project.name, ectx.system, ectx.arch
    );
    println!("mode           {mode}");
    println!(
        "builder        image={} base={} min_size={}B container_prefix={}",
        if p.builder.image_name.is_empty() { "-" } else { &p.builder.image_name },
        if p.builder.base_image.is_empty() { "-" } else { &p.builder.base_image },
        p.builder.min_image_size_bytes,
        p.container_name_prefix(),
    );
    println!(
        "strategy       default={} native_cache={} docker_volume={}",
        p.build_strategy.default,
        if p.build_strategy.native_cache.is_empty() { "-" } else { &p.build_strategy.native_cache },
        firestream_ci::platform::docker_cache_volume(
            &p.build_strategy.docker_cache_volume,
            &ectx.arch,
        ),
    );
    println!(
        "registry       {} containers ({})",
        p.known_containers().len(),
        if p.container_registry.is_empty() {
            "EMPTY — `firestream-ci build` cannot resolve a package name".to_string()
        } else {
            p.known_containers().join(", ")
        },
    );
    println!();

    match p.phase_order(mode) {
        Ok(order) => {
            println!("phase DAG (execution order for mode={mode}):");
            for ph in order {
                let deps = if ph.depends_on.is_empty() {
                    "-".to_string()
                } else {
                    ph.depends_on.join(",")
                };
                let modes = if ph.modes.is_empty() {
                    "*".to_string()
                } else {
                    ph.modes.join(",")
                };
                println!(
                    "  {:<8} tier={:<8} depends_on={:<14} modes={}{}{}",
                    ph.name,
                    ph.tier.label(),
                    deps,
                    modes,
                    if ph.aggregate { " aggregate" } else { "" },
                    if ph.export_artifacts { " export" } else { "" },
                );
                if !ph.has_work() {
                    println!("      (no work in this manifest — phase will not run)");
                }
                for t in &ph.builtin_tasks {
                    println!("      [builtin]  {t}");
                }
                for t in &ph.shell_tasks {
                    println!(
                        "      [shell]    {} -> {}",
                        t.name,
                        t.command
                            .iter()
                            .map(|a| p.expand(a, ectx))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                match p.phase_aggregate(&ph.name, ectx) {
                    Ok(Some((prefix, leaves))) => println!(
                        "      [aggregate] 1 nix-fast-build over {prefix} ({} attrs)",
                        leaves.len()
                    ),
                    Err(reason) => println!("      [aggregate] DISABLED: {reason}"),
                    Ok(None) => {}
                }
                for a in p.phase_attrs(&ph.name, ectx) {
                    // Phase-aware: an attr in an advisory phase is advisory.
                    println!(
                        "      [{}] {}",
                        p.tier_of_in_phase(&ph.name, &a, ectx).label(),
                        a
                    );
                }
                for a in p.phase_advisory_attrs(&ph.name, ectx) {
                    println!("      [advisory] {a}");
                }
            }
            // Phases excluded by the mode filter, so the omission is visible
            // rather than silent.
            for ph in &p.phases {
                if !ph.runs_in_mode(mode) {
                    println!("  {:<8} SKIPPED (modes={})", ph.name, ph.modes.join(","));
                }
            }
        }
        Err(e) => println!("phase DAG: ERROR {e}"),
    }

    println!();
    println!("tier rules (first match wins, default={}):", p.tier_default.label());
    if p.tier_rules.is_empty() {
        println!("  (none)");
    }
    for r in &p.tier_rules {
        println!("  {:<8} <- {}", r.tier.label(), describe_matcher(&r.matcher));
    }

    println!();
    println!("export targets (first match wins):");
    if p.export_targets.is_empty() {
        println!("  (none)");
    }
    for r in &p.export_targets {
        println!(
            "  {:<9} dest={:<28} <- {}",
            r.kind,
            r.dest,
            describe_matcher(&r.matcher)
        );
    }
    println!(
        "  {:<9} dest={:<28} <- (fallback)",
        p.export_default.kind, p.export_default.dest
    );

    println!();
    println!("passthrough vars ({}): {}", p.passthrough_vars.len(), p.passthrough_vars.join(" "));
    println!("devshell sentinels: {}", p.devshell_sentinel_summary());
    println!();
    println!("dry run: nothing executed.");
}

fn describe_matcher(m: &firestream_ci::profile::Matcher) -> String {
    if m.is_catch_all() {
        return "(any)".to_string();
    }
    let mut parts = Vec::new();
    if let Some(v) = &m.equals {
        parts.push(format!("equals={v:?}"));
    }
    if let Some(v) = &m.prefix {
        parts.push(format!("prefix={v:?}"));
    }
    if let Some(v) = &m.suffix {
        parts.push(format!("suffix={v:?}"));
    }
    if let Some(v) = &m.contains {
        parts.push(format!("contains={v:?}"));
    }
    parts.join(" & ")
}

fn print_ci_manifest(ctx: &CiLinuxCtx) {
    let host = if ctx.in_docker { "container" } else { "host" };
    let line = format!(
        "firestream-ci ci · {} · {} · {} · strategy={}",
        ctx.mode,
        ctx.nix_system,
        host,
        ctx.strategy.strategy.label()
    );
    print_ci_header(&line);
    if let Some(b) = &ctx.budget {
        firestream_ci::ci_emit!(
            "limits: {} MB RAM → pool {} MB → {} jobs × MemoryMax {} MB{}{}",
            b.total_mb,
            b.pool_mb,
            b.max_jobs,
            b.per_job_mb,
            match ctx.io_write_max_mb {
                Some(mb) => format!(", IOWriteBandwidthMax {mb} MB/s"),
                None => String::new(),
            },
            if b.jobs_reduced { " (concurrency reduced to fit memory)" } else { "" },
        );
    } else {
        firestream_ci::ci_emit!("limits: disabled (FIRESTREAM_CI_LIMITS=0)");
    }
    if let Some(b) = &ctx.strategy.blocker {
        firestream_ci::ci_emit!("strategy: native unavailable — {b}");
    }
}

/// One-line muted header printed before the live dashboard takes over.
/// Replaces the `=========` framed banner: less vertical noise, mode +
/// system + host on a single line so the user knows what they're running
/// without a paragraph to skim. ANSI dim escape is gated on TTY so piping
/// to a file (or `FIRESTREAM_CI_UI=banner` mode) doesn't smuggle escape codes
/// into the captured log.
fn print_ci_header(line: &str) {
    use std::io::IsTerminal;
    let color_ok = std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
    if color_ok {
        eprintln!("\x1b[2m{line}\x1b[0m\n");
    } else {
        eprintln!("{line}\n");
    }
}

async fn run_phases(
    ctx: &CiLinuxCtx,
    cancel: std::sync::Arc<tokio::sync::Notify>,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::pipeline::{Phase, Pipeline, Tier};

    // Select the reporter first so the typed dashboard handle (when
    // present) can be threaded through each phase's ctx clone. The
    // closures need that handle to wire `ring_sink` into FastBuild.
    let banner: std::sync::Arc<dyn firestream_ci::pipeline::Reporter> =
        std::sync::Arc::new(BannerReporter::new());
    let (reporter, dashboard, _ui_kind) =
        firestream_ci::dashboard::select_reporter(ctx.rundir.logs_dir().to_path_buf(), banner);

    let mut ctx_with_dash = ctx.clone();
    ctx_with_dash.dashboard = dashboard;
    let ctx = &ctx_with_dash;

    // Tier classification is profile data. Clone the `Arc` into the closure so
    // the pipeline can classify attrs it is handed at any point in the run.
    let tier_profile = ctx_with_dash.profile.clone();
    let mut builder = Pipeline::builder()
        .tier_classifier(move |attr: &str| tier_profile.tier_of(attr))
        .reporter(reporter);

    // ── The phase set, from the profile ─────────────────────────────────────
    //
    // `phase_order` is the SAME call `--dry-run` prints, so what you inspect is
    // what runs. Phases with no work in this mode are dropped rather than
    // emitted as empty green boxes; `ci_linux` has already hard-errored if
    // that leaves nothing at all.
    let runnable: Vec<String> = ctx
        .profile
        .runnable_phases(&ctx.mode)
        .into_iter()
        .map(|p| p.name.clone())
        .collect();
    let order = ctx
        .profile
        .phase_order(&ctx.mode)
        .map_err(|e| firestream_ci::Error::Other(format!("ci-linux: {e}")))?;

    for spec in order {
        if !runnable.contains(&spec.name) {
            continue;
        }
        let name = spec.name.clone();
        let tier = match spec.tier {
            firestream_ci::pipeline::Tier::Advisory => Tier::Advisory,
            _ => Tier::Required,
        };
        let mut phase = Phase::new(name.clone(), tier);
        // Only depend on phases that are actually in this run. A dependency
        // filtered out by mode (or with no work at this system) is treated as
        // satisfied — same rule `Profile::phase_order` applies.
        for dep in &spec.depends_on {
            if runnable.contains(dep) {
                phase = phase.depends_on(dep.clone());
            }
        }
        let phase_ctx = ctx_with_dash.clone();
        let phase_name = name.clone();
        builder = builder.phase(phase.parallel(move || phase_tasks(&phase_ctx, &phase_name)));
    }

    // Cancel-aware run: a SIGINT/SIGTERM (first signal) wakes the notifier
    // and we tear down the dashboard before returning a POSIX-conventional
    // 128+SIGINT exit code. A second signal force-exits via the handler
    // itself (see `install_signal_handler`). The outer scopeguard at the
    // ci-linux call site still fires on the early return and writes a
    // manifest with outcome=Aborted.
    let verdict = tokio::select! {
        v = builder.run() => v?,
        _ = cancel.notified() => {
            firestream_ci::dashboard::finalize_active_dashboard();
            firestream_ci::ci_emit!("firestream-ci ci-linux: aborted by signal");
            return Ok(130);
        }
    };
    // Tear down the dashboard before any subsequent stderr output so the
    // persisted last frame is the clean `RenderMode::Final` view rather
    // than whatever the tick thread happened to be rendering when the
    // process began winding down.
    firestream_ci::dashboard::finalize_active_dashboard();
    firestream_ci::ci_emit!();
    for line in verdict.summary_block_styled(want_color()).lines() {
        firestream_ci::ci_emit!("{line}");
    }

    Ok(u8::try_from(verdict.exit_code().clamp(0, 255)).unwrap_or(1))
}

/// ASCII pipeline reporter. Emits per-phase / per-task lifecycle lines as
/// the run unfolds, in a style consistent with the `====`/`----` banner.
/// Lines are written to stderr (alongside child process output streamed
/// via `LogSink::TeeTerminal`) so a single sink owns the user's terminal.
struct BannerReporter;

impl BannerReporter {
    fn new() -> Self {
        Self
    }
}

use firestream_ci::pipeline::display_task_name;

impl firestream_ci::pipeline::Reporter for BannerReporter {
    fn phase_start(&self, phase: &str, _tier: firestream_ci::pipeline::Tier, task_names: &[&str]) {
        // Mirror the dashboard's compact phase header: no `====` rule, no
        // tier text (tier surfaces in the post-run table). Status counter
        // starts at 0/N so the line is visually consistent with how the
        // dashboard would have rendered it.
        eprintln!();
        eprintln!("{phase:<8} 0/{}", task_names.len());
    }

    fn task_start(&self, _phase: &str, _task: &str) {
        // No-op: the dashboard never emits a per-task "started" line; the
        // banner reporter should match so log captures stay tight.
    }

    fn task_finish(
        &self,
        _phase: &str,
        task: &str,
        ok: bool,
        duration: std::time::Duration,
        error: Option<&str>,
    ) {
        let glyph = if ok { "+" } else { "x" };
        let dur = format_duration(duration);
        let shown = display_task_name(task);
        if ok {
            eprintln!("  {glyph} {shown:<20} {dur:>8}");
        } else {
            let detail = error.unwrap_or("(no error message)");
            eprintln!("  {glyph} {shown:<20} {dur:>8}  {detail}");
        }
    }

    fn phase_finish(&self, phase: &str, ok: bool, duration: std::time::Duration) {
        let verdict = if ok { "passed" } else { "failed" };
        let dur = format_duration(duration);
        eprintln!("{phase:<8} {verdict:<6} {dur:>8}");
    }

    fn phase_skipped(&self, phase: &str, _tier: firestream_ci::pipeline::Tier) {
        eprintln!();
        eprintln!("{phase:<8} skipped  (upstream required failed)");
    }
}

use firestream_ci::pipeline::format_duration;

/// Tier classifier: pure lookup into `profile.tier_rules` (ordered, first
/// match wins) with `profile.tier_default` as the fallback.
///
/// Replaces the compiled-in `required-*` / `advisory-*` prefix test. Both
/// mechanisms the reference used survive as data: a project that encodes tier
/// in its attr names writes `tier_rules`; a project whose check names carry no
/// tier convention (Firestream's `firestream-*`) lists the exceptions in the
/// phase's `advisory_attrs` instead. See `firestream_ci::profile`.
fn tier_for(ctx: &CiLinuxCtx, phase: &str, attr: &str) -> firestream_ci::pipeline::Tier {
    ctx.profile
        .tier_of_in_phase(phase, attr, &ctx.expand_ctx())
}

async fn run_tidy(ctx: &CiLinuxCtx) -> Result<(), firestream_ci::Error> {
    // gc runs in both docker and host-linux contexts. Previously the host
    // path silently no-op'd, which surfaced as a misleading `PASS nix-gc 0.0s`
    // line every run. Skip only when the user explicitly passes `--skip-gc`.
    if ctx.skip_gc {
        firestream_ci::ci_emit!("tidy: skipped (--skip-gc)");
        return Ok(());
    }
    // ── The host-store guard ────────────────────────────────────────────────
    //
    // Under native-first the CI cache and the developer's working store are
    // the SAME store: `build_strategy.native_cache = "host-store"` says so
    // explicitly. Reaping there is not cache maintenance, it is deleting
    // someone's working tree's dependencies mid-session. The standalone
    // `firestream-ci nix gc` subcommand already refuses without `--allow-host`;
    // this is the same refusal on the pipeline path, and it is deliberately
    // NOT overridable from inside a run — `auto` mode never touches a host
    // store, full stop.
    //
    // Native reclamation happens elsewhere and on the developer's terms: the
    // rundir GC roots are dropped when `rundir` prunes (age/count), and
    // `nix-collect-garbage` — run by the developer, or by
    // `make builder-cache-clean` — does the deleting.
    if !ctx.in_docker {
        firestream_ci::ci_emit!(
            "tidy: nix-gc skipped — host /nix/store (native_cache={}). \
             The host store is both the developer's working store and the \
             native build cache; reaping it from CI is never safe in `auto` \
             mode. Reclaim with `firestream-ci nix gc --allow-host` or \
             `nix-collect-garbage` after pruning _build/.",
            if ctx.profile.build_strategy.native_cache.is_empty() {
                "unset"
            } else {
                &ctx.profile.build_strategy.native_cache
            }
        );
        return Ok(());
    }
    // The check-mode PR gate should not front-load a full store GC walk
    // before its first actual check — phases run serially, so this sits on
    // the critical path. Store growth is bounded by the release runs; the
    // repo-local target-sweep task still runs in every mode.
    if ctx.mode != "release" {
        firestream_ci::ci_emit!(
            "tidy: nix-gc skipped (mode={} — store GC runs only in release mode)",
            ctx.mode
        );
        return Ok(());
    }
    // Collect-only tidy. The pre-rooting work previously done here
    // (enumerate every flake check/package, mass-realise their closures,
    // register an indirect root for each) was paying for verification of
    // tens of thousands of store paths even when GC ultimately deleted
    // nothing. With `--delete-older-than 7d`, anything registered in the
    // last week survives automatically, and per-build outputs are rooted
    // in `build_nix_attr_task` at the moment they finish — so tidy no
    // longer owns root creation.
    let ring_sink = ctx.dashboard.as_ref().map(|d| d.ring_for("tidy", "nix-gc"));
    let stderr_log = ctx.rundir.logs_dir().join("tidy-nix-gc.stderr.log");
    let report = firestream_ci::nix::collect_garbage("7d", Some(&stderr_log), ring_sink)?;
    // Tidy stats live in the phase summary (in parens after the verdict)
    // instead of as a separate `ci_emit!` line — that triplicated the
    // phase header (live block, phase_finish summary, gc stats line). A
    // green-but-empty gc (`0 deleted`) is the boring case; suppress the
    // parenthetical entirely so only interesting runs draw the eye.
    if report.deleted_paths.is_empty() {
        return Ok(());
    }
    if let Some(d) = ctx.dashboard.as_ref() {
        d.set_phase_detail("tidy", format!("{} deleted", report.deleted_paths.len()));
    }
    Ok(())
}

/// Tidy-phase wrapper over `run_target_sweep`: repo-local target/ + _build/
/// hygiene with the CI defaults (SWEEP_DAYS env override, keep 20 rundirs).
/// Runs in every mode — see the `Sweep` subcommand docs for why this is not
/// docker-gated like the store GC.
async fn run_tidy_sweep(_ctx: &CiLinuxCtx) -> Result<(), firestream_ci::Error> {
    let days = std::env::var("SWEEP_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let root = std::env::current_dir()
        .map_err(|e| firestream_ci::Error::Other(format!("target-sweep: resolving cwd: {e}")))?;
    run_target_sweep(&root, days, 20, false).await
}

/// Materialise the parallel task set for `phase`, entirely from the profile.
///
/// Three sources, in a fixed order so the dashboard column order is stable:
///
/// 1. `builtin_tasks` — the runner's own closed vocabulary (`nix-gc`,
///    `target-sweep`). These are the steps that are not derivations.
/// 2. `shell_tasks`   — free-form argv. The seam for anything non-hermetic.
/// 3. `attrs`         — nix attributes, either aggregated into a single
///    `FastBuild` (when the phase sets `aggregate`) or one task per attr.
///
/// Nothing here inspects the phase NAME. A project that calls its hygiene
/// phase `janitor` and its build phase `images` gets identical behaviour.
fn phase_tasks(ctx: &CiLinuxCtx, phase: &str) -> Vec<firestream_ci::pipeline::Task> {
    use firestream_ci::pipeline::Task;
    let mut tasks: Vec<Task> = Vec::new();
    let Some(spec) = ctx.profile.phase(phase) else {
        return tasks;
    };

    for builtin in &spec.builtin_tasks {
        let c = ctx.clone();
        let sp = ctx.child_span(phase, builtin);
        match builtin.as_str() {
            "nix-gc" => tasks.push(Task::new("nix-gc", async move {
                finish_span(sp, run_tidy(&c).await.map_err(|e| e.to_string()))
            })),
            "target-sweep" => tasks.push(Task::new("target-sweep", async move {
                finish_span(sp, run_tidy_sweep(&c).await.map_err(|e| e.to_string()))
            })),
            // Unreachable: `Profile::validate` rejects unknown builtins at
            // load time. Kept total so a future vocabulary addition that
            // forgets a match arm is a loud failure, not a silent skip.
            other => {
                let msg = format!("unknown builtin task `{other}`");
                tasks.push(Task::new(other.to_string(), async move { Err(msg) }));
            }
        }
    }

    let ectx = ctx.expand_ctx();
    for st in &spec.shell_tasks {
        let name = st.name.clone();
        let argv: Vec<String> = st
            .command
            .iter()
            .map(|a| ctx.profile.expand(a, &ectx))
            .collect();
        let log = ctx
            .rundir
            .logs_dir()
            .join(format!("{phase}-{}.log", sanitize_name(&name)));
        let sp = ctx.child_span(phase, &name);
        let prefix = name.clone();
        tasks.push(Task::new(name, async move {
            let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            finish_span(sp, run_logged_shell(&prefix, &argv[0], &args, &log).await)
        }));
    }

    tasks.extend(attr_tasks(ctx, phase));
    tasks
}

/// The nix-attribute half of [`phase_tasks`]: either one aggregated
/// `FastBuild` over the attrs' common prefix, or one per attr.
fn attr_tasks(ctx: &CiLinuxCtx, phase: &str) -> Vec<firestream_ci::pipeline::Task> {
    let ectx = ctx.expand_ctx();
    match ctx.profile.phase_aggregate(phase, &ectx) {
        Ok(Some((prefix, leaves))) => {
            // ONE nix-fast-build: one flake evaluation, one bounded job queue,
            // per-attr entries still in the result file. `--select` narrows
            // `packages.<system>` (or `checks.<system>`) to exactly the
            // profile's leaves, so nothing outside the declared set is built.
            let select = firestream_ci::nix::select_expr_for_leaves(&leaves);
            vec![build_nix_attr_task_named(
                ctx,
                prefix,
                phase,
                format!("{phase}-all"),
                Some(select),
            )]
        }
        Ok(None) => phase_attr_list(ctx, phase)
            .into_iter()
            .map(|attr| build_nix_attr_task(ctx, attr, phase))
            .collect(),
        Err(reason) => {
            // Degrade, loudly, to per-attr. An unbuildable profile should not
            // be the failure mode of a misconfigured aggregation flag.
            firestream_ci::ci_emit!(
                "{phase}: aggregation disabled ({reason}); falling back to per-attr builds"
            );
            phase_attr_list(ctx, phase)
                .into_iter()
                .map(|attr| build_nix_attr_task(ctx, attr, phase))
                .collect()
        }
    }
}

/// Required + advisory attrs of `phase`, in that order, expanded.
///
/// `{system}` / `{arch}` / `{project}` are expanded against the *runtime*
/// context (`--nix-system` / `--arch` win over the profile's baked values).
/// There is no arch gating here and there never will be: a profile that must
/// differ per system is emitted per system by Nix.
fn phase_attr_list(ctx: &CiLinuxCtx, phase: &str) -> Vec<String> {
    let ectx = ctx.expand_ctx();
    let mut attrs = ctx.profile.phase_attrs(phase, &ectx);
    attrs.extend(ctx.profile.phase_advisory_attrs(phase, &ectx));
    attrs
}

/// Stamp a task's verdict onto its span and close it, so `report summary`'s
/// Outcome column carries a real value instead of OTel's `unset` default.
/// Returns the result unchanged so it can wrap a tail expression.
fn finish_span(
    span: Option<firestream_ci::trace::Span>,
    res: Result<(), String>,
) -> Result<(), String> {
    use firestream_ci::trace::SpanStatus;
    if let Some(mut s) = span {
        match &res {
            Ok(()) => s.set_status(SpanStatus::Ok),
            Err(e) => {
                s.set_status(SpanStatus::Error);
                s.set_status_message(e.clone());
            }
        }
        s.finish();
    }
    res
}

/// Filesystem-safe rendering of a task name for log file paths.
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Build a task that drives one nix-fast-build attr invocation. Captures
/// the per-attr result file, synthesises a failure entry when nix-fast-build
/// exits before writing results (mirrors `bin/ci/ci-linux.sh:268-276`), then
/// reconciles spans. `build` phase additionally exports the resulting artifact.
fn build_nix_attr_task(
    ctx: &CiLinuxCtx,
    attr: String,
    phase_tag: &str,
) -> firestream_ci::pipeline::Task {
    let task_name = attr_leaf(&attr).to_string();
    build_nix_attr_task_named(ctx, attr, phase_tag, task_name, None)
}

/// Like [`build_nix_attr_task`], but with an explicit task display name and an
/// optional `--select` expression — the aggregated path, where `attr` is a
/// whole attrset (`packages.<system>`) whose leaf would otherwise render as the
/// bare system name.
///
/// Before Phase 6 this took a hardcoded `ci-verify.<system>` attr that this
/// flake never defined, so the default verify path could not succeed. The
/// prefix now comes from the profile's own attr list
/// ([`firestream_ci::profile::Profile::phase_aggregate`]) and the `--select`
/// narrows it back to exactly those attrs, so the aggregate cannot drift from
/// the declaration.
fn build_nix_attr_task_named(
    ctx: &CiLinuxCtx,
    attr: String,
    phase_tag: &str,
    task_name: String,
    select_expr: Option<String>,
) -> firestream_ci::pipeline::Task {
    use firestream_ci::nix::{FastBuild, append_synth_failures, synth_failure};
    use firestream_ci::pipeline::Task;

    let phase_tag = phase_tag.to_string();
    // Whether this phase's outputs are release artifacts is profile data
    // (`export_artifacts`), not a comparison against the string "build".
    let export_artifacts = ctx
        .profile
        .phase(&phase_tag)
        .map(|p| p.export_artifacts)
        .unwrap_or(false);
    // Cgroup budget: derived once per run from physical RAM. `max_jobs` is
    // reduced until each concurrent child can hold at least the 2 GB floor, so
    // the aggregate of N scopes stays inside the pool rather than being N ×
    // MemoryMax. On the documented 8 GB devcontainer minimum this is 3 jobs ×
    // ~2 GB, not 4 × 6 GB.
    let budget = ctx.budget.clone();
    let io_write_max_mb = ctx.io_write_max_mb;
    let span = ctx.child_span(&phase_tag, &task_name);

    let log_dir = ctx.rundir.logs_dir().to_path_buf();
    let span_dir = ctx.rundir.spans_dir().to_path_buf();
    let nix_system = ctx.nix_system.clone();
    let arch = ctx.arch.clone();
    let mode = ctx.mode.clone();
    let rundir = ctx.rundir.clone();
    let artifacts_format = ctx.artifacts_format;
    let artifacts_max_bytes = ctx.artifacts_max_bytes;
    let profile = ctx.profile.clone();
    // Tier is resolved here, at the call site, so the closure carries a plain
    // value rather than the profile lookup. `tier_for` consults the phase's
    // `advisory_attrs` first, then `tier_rules` / `tier_default`.
    let attr_tier = tier_for(ctx, &phase_tag, &attr);

    // The task display name is normally the full leaf attr (see
    // build_nix_attr_task). Earlier revisions stripped `required-` for
    // readability, but that asymmetry (advisory tasks kept their prefix) was
    // the source of the dashboard's double-`advisory-` path-build bug — the
    // writer named files by the full leaf, the reader rebuilt them via the
    // stripped task name plus tier label. Keeping one name throughout makes
    // it the single source of truth for both the file and the display column.
    // When the dashboard is active, allocate a tail-N ring for this task
    // BEFORE the closure runs (`task_start` fires later — by then the
    // FastBuild builder has already been constructed). The dashboard
    // claims the ring on `task_start` keyed by (phase, task).
    let ring_sink = ctx
        .dashboard
        .as_ref()
        .map(|d| d.ring_for(&phase_tag, &task_name));
    let closure_name = task_name.clone();
    Task::new(task_name, async move {
        let phase_tag = phase_tag.as_str();
        let task_name = closure_name;
        let safe_attr = attr.replace(['.', '/'], "_");
        let result_file = nix_fast_build_result_path(&log_dir, phase_tag, &safe_attr);
        // Per-attr summary log replaces the previously shared
        // `nix-fast-build-{phase}.log` (a last-writer-wins race target
        // across 8+ concurrent attrs).
        let log_path = nix_fast_build_log_path(&log_dir, phase_tag, &safe_attr);
        // Per-attr stderr log. Points the user at the actual nix-fast-build
        // failure cause when it exits before writing its JSON result file.
        // Tier is derived here so the helper signature can pin the
        // classification at the call site, even though it does not affect
        // the on-disk filename (the tier is already in the task name).
        let leaf = task_name.as_str();
        let stderr_log = stderr_log_path(&log_dir, phase_tag, attr_tier, leaf);

        // Drive the build via the typed FastBuild builder. Keep the flake
        // URL and the attribute fragment separate — `firestream-nix-build`
        // joins them with `#` internally, so stuffing the fragment into
        // `flake()` would yield a trailing-`#` URL that Nix rejects.
        let max_jobs = budget
            .as_ref()
            .map(|b| b.max_jobs)
            .unwrap_or_else(|| env_usize("NIX_BUILD_MAX_JOBS").unwrap_or(DEFAULT_MAX_JOBS));
        let mut fb = FastBuild::builder()
            .flake(".")
            .attr(&attr)
            .system(&nix_system)
            // Concurrency is the memory budget's decision when limits are on
            // (see `BuildBudget::derive`); otherwise the historical default.
            .max_jobs(max_jobs)
            .cores(env_usize("NIX_BUILD_CORES").unwrap_or(0))
            .keep_going(true)
            .skip_cached(true)
            .no_link(true)
            .result_file(result_file.clone())
            .stderr_log(stderr_log.clone())
            .spans_dir(span_dir.clone());
        if let Some(sel) = select_expr {
            fb = fb.select_expr(sel);
        }
        if let Some(b) = &budget {
            // Every `nix build` child runs inside its own
            // `systemd-run --user --scope` with MemoryMax/MemoryHigh and the
            // io.max write cap. Falls through unwrapped (with a warn) when
            // systemd is unavailable — e.g. inside the docker builder.
            fb = fb.limits(b.per_job_limits(io_write_max_mb));
        }
        if let Some(r) = ring_sink {
            fb = fb.ring_sink(r);
        }
        let run_result = fb
            .build()
            .map_err(|e| format!("{phase_tag}:{attr}: builder: {e}"))?
            .run()
            .await;

        let exit_code = match &run_result {
            Ok(r) => r.exit_code,
            Err(_) => 1,
        };

        // When the FastBuild driver itself errored (e.g., nix-eval-jobs not
        // found, flake eval crashed) the nix child never reached the
        // `run_nix_build` tee path so `stderr_log` is empty. Append the
        // driver error so the user has *something* concrete on disk.
        if let Err(e) = &run_result {
            if let Some(parent) = stderr_log.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&stderr_log, format!("nix-fast-build driver error:\n{e}\n"));
        }

        // Synth failure if the result file is missing or empty (bash lines
        // 268-275 / 513-518). Reconcile (bash lines 283-288 / 522-527).
        // Capture a user-shaped cause now so it can flow into the task's
        // error message — the pipeline reporter renders one [FAIL] line
        // per task, so we do NOT eprintln here.
        let mut synth_cause: Option<String> = None;
        if !result_file.exists()
            || std::fs::metadata(&result_file)
                .map(|m| m.len() == 0)
                .unwrap_or(true)
        {
            synth_cause = Some(classify_synth_cause(
                &result_file,
                exit_code,
                phase_tag,
                leaf,
            ));
            let reason = format!("nix-fast-build exited {exit_code} with no result file");
            let _ = append_synth_failures(&result_file, &[synth_failure(&attr, &reason)]);
        }

        // Reconcile spans with the authoritative JSON verdict (advisory —
        // failure leaves the heuristic-derived status in place).
        let _ = firestream_ci::nix::reconcile(&result_file, &span_dir).await;

        // Count failed entries.
        let failed = count_failed_results(&result_file);
        let _ = std::fs::write(
            log_path,
            format!("attr={attr} exit={exit_code} failed={failed}\n"),
        );

        // Export + record a manifest entry for every successful output, when
        // the profile marks this phase's outputs as release artifacts. The
        // aggregated path produces MANY entries in one result file, so this
        // walks all of them rather than taking the first.
        if export_artifacts && failed == 0 && exit_code == 0 {
            let n = firestream_ci::artifacts::export_result_file(
                &profile,
                &attr,
                &arch,
                &rundir,
                &result_file,
                artifacts_format,
                artifacts_max_bytes,
            )
            .await;
            tracing::debug!(phase = phase_tag, exported = n, "artifacts exported");
        }

        // Register a persistent indirect GC root for each output of this
        // attr — for EVERY phase, not just `build`. Tidy is collect-only
        // (see `run_tidy`): it runs `nix-collect-garbage --delete-older-than
        // 7d`, and that flag only prunes old *profile generations* — the
        // store sweep that follows deletes every path not reachable from a
        // root, regardless of how recently it was built. Verify outputs are
        // produced with `--no-link` (no result symlink, hence no root), so
        // without this they are reaped by the NEXT run's tidy and rebuilt
        // from scratch — the multi-hour clippy/nextest/doc/ts-* penalty on
        // every run. Rooting them here at the moment the attr finishes keeps
        // the cache warm across runs. Best-effort: failures are logged
        // inside `register_indirect_roots` via `tracing::warn!` and do not
        // fail the task.
        if failed == 0 && exit_code == 0 {
            let root_items = collect_build_root_items(&result_file, &safe_attr);
            if !root_items.is_empty() {
                let _ = firestream_ci::nix::register_indirect_roots(
                    std::path::Path::new("/tmp/ci-gc-roots"),
                    &root_items,
                );
            }
        }

        // Mode is only consulted to suppress reporting in dry-run paths;
        // not used today but kept so the closure has the same env shape as
        // bash's per-attr loop.
        let _ = mode;

        let outcome = if failed > 0 || exit_code != 0 {
            // Prefer the user-shaped cause from synth detection over the
            // mechanical `(exit=…, failed_entries=…)` tuple, then append
            // the tail of the per-attr stderr log so the user sees the
            // actual failure (e.g. the `cargo fmt --check` diff) inline
            // instead of having to chase a log path. Pure cosmetics for
            // the synth-cause path; load-bearing for the generic path.
            let base = synth_cause.unwrap_or_else(|| {
                format!("build failed (exit {exit_code}, {failed} failed entries)")
            });
            Err(format_task_failure(&base, &stderr_log, leaf))
        } else {
            Ok(())
        };
        finish_span(span, outcome)
    })
}

/// Compose the final per-task error message: base cause + a tail of the
/// stderr log + an optional leaf-specific hint (e.g. `cargo fmt`).
///
/// Kept narrow so it can be unit-tested without spinning up nix-fast-build.
/// Whether the verdict summary should emit ANSI color. Mirrors the dashboard's
/// gate (`dashboard::select_reporter`): color only on an interactive stderr,
/// and never when `NO_COLOR`, a dumb terminal, or `CI` say otherwise. `ci_emit`
/// routes to stderr, so that is the stream we probe.
fn want_color() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
        && std::env::var_os("CI").is_none()
}

fn format_task_failure(base: &str, stderr_log: &std::path::Path, leaf: &str) -> String {
    let mut msg = base.to_string();
    if let Some(tail) = read_log_tail(stderr_log, 12) {
        msg.push_str(&format!("\nlog: {}\n{}", stderr_log.display(), tail));
    }
    if let Some(hint) = leaf_hint(leaf) {
        msg.push('\n');
        msg.push_str(hint);
    }
    msg
}

/// Drop ANSI/VT escape sequences from `s`. Nix and cargo emit colorized
/// stderr; echoing the raw bytes prints literal `[1m…` garbage in a
/// non-terminal sink (and breaks the verdict table's width math). We strip at
/// the source — the log tail — so every downstream consumer gets clean text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI (`ESC [ … final`) and the common `ESC ] … BEL/ST` OSC forms.
        match chars.peek() {
            Some('[') => {
                chars.next();
                // Params/intermediates until a final byte in 0x40..=0x7e.
                for b in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&b) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC runs to BEL or ESC\.
                while let Some(b) = chars.next() {
                    if b == '\u{07}' {
                        break;
                    }
                    if b == '\u{1b}' {
                        chars.next(); // consume the trailing '\'
                        break;
                    }
                }
            }
            // Lone ESC or two-byte escape: drop ESC, keep the next char.
            _ => {}
        }
    }
    out
}

/// Progress chatter that drowns the real failure when a tail is shown: the
/// hundreds of `core-types: Copying …` build-script notices, `Compiling`/
/// `Finished` cargo lines, and nix phase banners. Dropping these *before*
/// tailing surfaces the lines that actually explain the failure (e.g.
/// `mkdir: cannot create directory …: File exists`).
fn is_progress_noise(line: &str) -> bool {
    let t = line.trim_start();
    t.contains(": Copying ")
        || t.starts_with("warning: ")
        || t.starts_with("Compiling ")
        || t.starts_with("Finished ")
        || t.starts_with("Running phase:")
        || t.starts_with("Building ")
        || t.is_empty()
}

/// Read the last `n_lines` *signal* lines of `path`: ANSI-stripped, with
/// pure-progress chatter filtered out first so the tail lands on the real
/// error rather than the last few `Copying …` notices. Returns `None` if the
/// file is missing or empty. Bounded memory: a per-attr stderr log is small
/// (< 1 MiB in practice), so read-whole-then-trim stays obvious.
fn read_log_tail(path: &std::path::Path, n_lines: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .lines()
        .map(strip_ansi)
        .filter(|l| !is_progress_noise(l))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(n_lines);
    Some(lines.split_off(start).join("\n"))
}

/// Per-leaf fix-it hint. Only fires for leaves with a single, well-known
/// remediation — generic "see logs" guidance lives in `format_task_failure`.
fn leaf_hint(leaf: &str) -> Option<&'static str> {
    match leaf {
        "required-rust-fmt" => Some("hint: run `cargo fmt` to fix formatting"),
        "required-ts-format" => Some("hint: run `make format` to fix TS/CSS formatting"),
        _ => None,
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
}

fn attr_leaf(attr: &str) -> &str {
    attr.rsplit('.').next().unwrap_or(attr)
}

/// Render the user-visible cause when `nix-fast-build` exited before
/// writing its result file. Points at the per-attr stderr log so the user
/// can find the real failure reason.
fn classify_synth_cause(
    result_file: &std::path::Path,
    exit_code: u8,
    phase_tag: &str,
    leaf: &str,
) -> String {
    let stderr_log_hint = format!("logs/{phase_tag}-{leaf}.stderr.log");
    if exit_code == 137 {
        return format!("OOM killed (137); see {stderr_log_hint}");
    }
    // File exists but empty vs missing entirely: the empty case means
    // nix-fast-build opened it then exited before writing any entries.
    if result_file.exists() {
        format!("build produced no output (exit {exit_code}); see {stderr_log_hint}")
    } else {
        format!("nix-fast-build exited {exit_code} before writing results; see {stderr_log_hint}")
    }
}

fn count_failed_results(result_file: &std::path::Path) -> usize {
    let bytes = match std::fs::read(result_file) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let doc: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    doc.get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| e.get("success").and_then(|v| v.as_bool()) == Some(false))
                .count()
        })
        .unwrap_or(0)
}

/// Parse a nix-fast-build result file and return `(symlink_name,
/// storepath)` pairs for every output of every *successful* entry. The
/// symlink name is `<safe_attr>__<output_name>` so multi-output
/// derivations get one root per output. Used by the build phase to
/// register an indirect GC root for each just-built attr so the next
/// tidy phase's `nix-collect-garbage --delete-older-than 7d` cannot reap
/// it. Returns empty on any IO/parse failure — callers treat root
/// registration as best-effort.
fn collect_build_root_items(
    result_file: &std::path::Path,
    safe_attr: &str,
) -> Vec<(String, String)> {
    let bytes = match std::fs::read(result_file) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let doc: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = doc.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in arr {
        if entry.get("success").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        let Some(outputs) = entry.get("outputs").and_then(|v| v.as_object()) else {
            continue;
        };
        for (output_name, storepath_val) in outputs {
            if let Some(storepath) = storepath_val.as_str() {
                items.push((format!("{safe_attr}__{output_name}"), storepath.to_string()));
            }
        }
    }
    items
}

async fn run_logged_shell(
    prefix: &str,
    program: &str,
    args: &[&str],
    log: &std::path::Path,
) -> Result<(), String> {
    use firestream_ci::exec::Command as ExecCommand;
    let mut cmd = ExecCommand::new(program);
    for a in args {
        cmd = cmd.arg(*a);
    }
    cmd = cmd.span(format!("exec:{}", program));
    // Tee streams the child's merged stdout+stderr to the terminal as it
    // runs, with each line prefixed `[<prefix>] `, and persists the same
    // bytes (un-prefixed) to `log` for post-hoc inspection.
    cmd = cmd.tee_terminal(prefix, log.to_path_buf());
    let report = cmd
        .run_unsupervised()
        .await
        .map_err(|e| format!("{program}: spawn: {e}"))?;

    if report.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited {} ({})",
            report.exit_code, report.code_description
        ))
    }
}

// NOTE on the retired hardcoded `e2e` phase.
//
// Until Phase 6 this file appended a fifth, Rust-only `e2e` phase running
// `bash ./bin/test/e2e_server.sh` — a path inherited from the reference
// project that does not exist in this repo, so on a release run it failed
// every time and pinned the verdict at PartiallyPassed (exit 2) forever. It
// also had no profile counterpart, which is precisely the coupling the payload
// design exists to remove.
//
// It is not dropped, it is generalised: `shell_tasks` on any phase runs an
// arbitrary argv as a parallel task with the same logging, span and tier
// treatment.
//
// Phase 9 took delivery of that seam and got the phase back — with NO Rust
// change, which was the point. `bin/nix/firestream/ci/profile.nix` now
// declares 18 advisory, single-shell-task phases (`e2e-docker-<stack>` x8,
// `e2e-k8s-<chart>` x10) chained head-to-tail with `dependsOn` and gated
// behind `modes = [ "e2e" ]`.
//
// Two deliberate departures from the sketch above, both forced by facts:
//
//   * `modes = [ "release" ]` -> `modes = [ "e2e" ]`. A release run must not
//     acquire an hours-long, cluster-creating tail. `--mode` is a free-form
//     string here (see `build_ci_linux_ctx`), so a third mode costs nothing.
//   * ONE phase with many parallel tasks -> MANY chained single-task phases.
//     `Pipeline::run` fans a phase's tasks out with `join_all` and no
//     concurrency cap, while both harnesses hold a process-wide mutex
//     precisely because they create real k3d clusters and bind host ports.
//     Chaining advisory phases reproduces that serialisation using only
//     `depends_on`, and — because only a failing *required* upstream skips a
//     dependent — a broken chart still does not stop the sweep.
//
// Invariants proved from JSON alone in `tests/e2e_phase_chain.rs`.

async fn finalize_manifest<C: RunContext + ?Sized>(
    ctx: &C,
    outcome: Outcome,
) -> Result<(), firestream_ci::Error> {
    use firestream_ci::manifest::{Manifest, RunMeta};
    let rundir = ctx.rundir();

    // Build the caller-side RunMeta. The Manifest::finalize() field-merge
    // (see manifest/mod.rs) fills in any blanks from `infer_run_meta(rundir)`
    // — so `host_os`/`arch`/`date`/`sha`/`epoch` need only be supplied here
    // when the caller knows them better than the rundir layout does.
    // Branch helper (same as the one used at `k8s namespace`): falls back
    // to "" if detached HEAD / no env hint — that's intentional, the merge
    // in Manifest::finalize() will accept the empty and leave whatever
    // `infer_run_meta` produced (also "", since there's no rundir hint
    // for branch). The point is to avoid forcing a hard error on a
    // perfectly valid local CI run.
    let branch = resolve_branch_for_k8s().unwrap_or_default();
    let mode = std::env::var("CI_MODE").unwrap_or_default();
    let meta = RunMeta {
        branch,
        mode,
        host_os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        outcome: Some(outcome),
        ..Default::default()
    };

    let m = Manifest::open(rundir).await?.with_run_meta(meta);
    let _path = m.finalize().await?;

    // Aggregated span NDJSON. Sits next to the markdown summary in
    // `<rundir>/profiles/` so external consumers can `jq` over the run's
    // spans without re-walking the `<trace>/<span>/span.json` tree. Best
    // effort — finalize MUST NOT fail because spans were unreadable; the
    // canonical record on disk is the manifest, the NDJSON is a
    // convenience-derived view.
    let spans_dir = rundir.spans_dir();
    match Report::from_span_dir(spans_dir) {
        Ok(report) => {
            let ndjson_path = rundir.profiles_dir().join("spans.ndjson");
            if let Err(e) = report.write_ndjson(&ndjson_path) {
                eprintln!(
                    "manifest: spans NDJSON write failed (non-fatal): {} ({e})",
                    ndjson_path.display()
                );
            }
        }
        Err(e) => {
            eprintln!(
                "manifest: report scan of {} failed (non-fatal): {e}",
                spans_dir.display()
            );
        }
    }

    // Per-phase summaries replace the previously shared
    // `nix-fast-build-{phase}.log` (a last-writer-wins race target). For each
    // phase we observe per-attr result JSONs from, write
    // `<rundir>/profiles/<phase>.json` with one entry per attr. Descriptive
    // like the manifest — read failures log + skip, never abort finalize.
    let logs_dir = rundir.logs_dir().to_path_buf();
    let profiles_dir = rundir.profiles_dir().to_path_buf();
    if let Err(e) = write_phase_summaries(&logs_dir, &profiles_dir) {
        eprintln!("manifest: phase summary write failed (non-fatal): {e}");
    }

    Ok(())
}

/// Auto-replay spans to Honeycomb at end-of-run. Wired into every runner's
/// success path. Decision rule:
///
/// 1. `HONEYCOMB_API_KEY` must be set in env. Without it, return early —
///    the offline path is byte-identical to today.
/// 2. `no_export_honeycomb=true` opts out unconditionally.
/// 3. Outcomes:
///    - `Outcome::Passed`: ship.
///    - `Outcome::Failed`: ship only when `export_honeycomb_on_failure=true`.
///    - `Outcome::Aborted`: never ship (partial spans aren't actionable).
///
/// All failures are logged and swallowed — Honeycomb being unreachable
/// must never fail a green run. The single summary line is
/// `honeycomb: shipped N spans (failed M)`.
async fn maybe_export_honeycomb(
    rundir: &RunDir,
    outcome: Outcome,
    no_export_honeycomb: bool,
    export_honeycomb_on_failure: bool,
) {
    use firestream_ci::checkpoint::replay_dir_to_honeycomb;

    if no_export_honeycomb {
        return;
    }
    if std::env::var("HONEYCOMB_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        // Offline path — no API key means nothing to ship and no warning;
        // the operator opted out by not providing credentials.
        return;
    }
    let should_ship = match outcome {
        Outcome::Passed => true,
        Outcome::Failed => export_honeycomb_on_failure,
        Outcome::Aborted => false,
    };
    if !should_ship {
        return;
    }

    let spans_dir = rundir.spans_dir().to_path_buf();
    match replay_dir_to_honeycomb(&spans_dir).await {
        Ok(r) => eprintln!(
            "honeycomb: shipped {} spans (failed {})",
            r.shipped, r.failed
        ),
        Err(e) => eprintln!("honeycomb: replay failed (non-fatal): {e}"),
    }
}

/// Roll per-attr `nix-fast-build-{phase}.{safe_attr}.json` files in `logs_dir`
/// up into one `<profiles_dir>/<phase>.json` summary per phase. Replaces the
/// pre-refactor shared `nix-fast-build-{phase}.log` whose single line was
/// trampled by the last attr to finish. Best-effort: missing files / parse
/// errors are logged + skipped, never propagated.
fn write_phase_summaries(
    logs_dir: &std::path::Path,
    profiles_dir: &std::path::Path,
) -> std::io::Result<()> {
    use std::collections::BTreeMap;

    if !logs_dir.is_dir() {
        return Ok(());
    }
    // phase -> Vec<(attr_display, exit, failed, result_file_abs)>
    let mut by_phase: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for entry in std::fs::read_dir(logs_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Match `nix-fast-build-{phase}.{safe_attr}.json`.
        let Some(rest) = fname.strip_prefix("nix-fast-build-") else {
            continue;
        };
        let Some(stem) = rest.strip_suffix(".json") else {
            continue;
        };
        // First dot splits `{phase}` from `{safe_attr}`. `safe_attr` may
        // contain additional underscores but no dots (`.` was replaced).
        let Some((phase, safe_attr)) = stem.split_once('.') else {
            continue;
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "phase-summary: read {} failed (skipped): {e}",
                    path.display()
                );
                continue;
            }
        };
        let doc: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "phase-summary: parse {} failed (skipped): {e}",
                    path.display()
                );
                continue;
            }
        };
        let failed = doc
            .get("results")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| e.get("success").and_then(|v| v.as_bool()) == Some(false))
                    .count()
            })
            .unwrap_or(0);
        // The bash + Rust callers pass the full attr (e.g.
        // `checks.x86_64-linux.required-rust-fmt`) through safe-attr
        // encoding. Recovering it would require knowing the system; the
        // safe-attr form is unambiguous on disk and good enough for a
        // summary view.
        let entry_val = serde_json::json!({
            "attr": safe_attr,
            "failed": failed,
            "result_file": path.to_string_lossy(),
        });
        by_phase
            .entry(phase.to_string())
            .or_default()
            .push(entry_val);
    }
    if by_phase.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(profiles_dir)?;
    for (phase, attrs) in by_phase {
        let payload = serde_json::json!({
            "phase": phase,
            "attrs": attrs,
        });
        let out = profiles_dir.join(format!("{phase}.json"));
        let tmp = out.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&payload).unwrap_or_else(|_| b"{}".to_vec());
        if let Err(e) = std::fs::write(&tmp, &body) {
            eprintln!(
                "phase-summary: write {} failed (skipped): {e}",
                tmp.display()
            );
            continue;
        }
        if let Err(e) = std::fs::rename(&tmp, &out) {
            eprintln!(
                "phase-summary: rename {} -> {} failed (skipped): {e}",
                tmp.display(),
                out.display()
            );
            continue;
        }
    }
    Ok(())
}

fn write_summary(ctx: &CiLinuxCtx) -> Result<(), firestream_ci::Error> {
    use firestream_ci::report::Report;
    let span_dir = ctx.rundir.spans_dir();
    if !span_dir.is_dir() {
        return Ok(());
    }
    let report = Report::from_span_dir(span_dir)?;
    let profile_dir = ctx.rundir.profiles_dir();
    let out = profile_dir.join("ci-summary.md");
    report.summary_markdown(&out)?;
    Ok(())
}

/// One trailing block listing every artifact this run produced, with
/// paths relative to the rundir root. Replaces three scattered inline
/// `attest:` / `manifest:` / `CI summary:` lines that printed full
/// absolute paths mid-stream. Skips entirely if no artifacts exist.
fn print_artifacts_footer(rundir: &RunDir) {
    let root = rundir.path();
    let probes: &[(&str, std::path::PathBuf)] = &[
        ("manifest", root.join("manifest.json")),
        ("sbom", rundir.artifacts_dir().join("sbom")),
        ("summary", rundir.profiles_dir().join("ci-summary.md")),
        ("spans", rundir.profiles_dir().join("spans.ndjson")),
    ];
    let entries: Vec<(&str, String)> = probes
        .iter()
        .filter_map(|(label, path)| {
            if !path.exists() {
                return None;
            }
            path.strip_prefix(root)
                .ok()
                .map(|rel| (*label, rel.display().to_string()))
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    let width = entries.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    firestream_ci::ci_emit!();
    firestream_ci::ci_emit!("Artifacts ({}):", root.display());
    for (label, rel) in entries {
        firestream_ci::ci_emit!("  {:<width$}  {}", label, rel, width = width);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci ci` — top-level dispatcher
// ───────────────────────────────────────────────────────────────────────────
//
// Maps `CI_RUNNER` (or auto-detected runner) to the corresponding sub-runner
// subcommand on this same binary, then re-execs `firestream-ci <sub>`. The bash
// dispatcher had the same shape — pick a runner script, `exec` it.

async fn ci_dispatch(
    args: CiDispatchArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::runner::Runner;

    // Explicit flag wins; then CI_RUNNER env (handled inside Runner::detect);
    // then sentinel-based auto-detection using the profile's
    // `devshell_sentinels`.
    let runner = if let Some(r) = args.runner.as_deref() {
        Runner::parse(r).map_err(firestream_ci::Error::from)?
    } else {
        Runner::detect_with_profile(profile).map_err(|e| {
            // Mirror the bash exit-2-with-help-text behaviour. Surface the
            // detection error so callers can see why detection failed.
            eprintln!("firestream-ci ci: {e}");
            eprintln!(
                "firestream-ci ci: no viable CI runner. Install Nix (for host-linux) or Docker,"
            );
            eprintln!(
                "          or set CI_RUNNER={{host-linux,host-darwin,docker,cloudbuild}} explicitly."
            );
            firestream_ci::Error::Other(format!("dispatch detection failed: {e}"))
        })?
    };

    let sub = match runner {
        Runner::HostLinux => "ci-linux",
        // The Darwin runner shell is kept (it is the "build Linux images from
        // a macOS host" path) but its ConceptDB Xcode/iOS body was removed in
        // the Firestream lift and no Firestream-shaped body exists yet.
        Runner::HostDarwin => {
            eprintln!(
                "firestream-ci ci: detected the host-darwin runner, but no Darwin pipeline body \n\
                 is implemented in firestream-ci yet. Use `CI_RUNNER=docker` (Linux images are \n\
                 built from macOS through the docker runner) or run `firestream-ci ci-linux` \n\
                 inside a Linux devshell."
            );
            return Err(firestream_ci::Error::Other(
                "host-darwin runner has no pipeline body".into(),
            ));
        }
        Runner::Docker => "ci-docker",
        Runner::Cloudbuild => "ci-cloudbuild",
    };

    if args.dry_run {
        eprintln!(
            "firestream-ci ci: would dispatch to `{sub}` (runner={}, forward={:?})",
            runner.label(),
            args.forward
        );
        // Then the profile itself: phase DAG, tiers, resolved attrs. Nothing
        // is executed — this is the acceptance hook for the Phase 4 contract.
        let mode = std::env::var("CI_MODE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "release".to_string());
        eprintln!();
        print_profile_dry_run(profile, &mode, &profile.expand_ctx());
        return Ok(0);
    }

    tracing::debug!(runner = runner.label(), sub, "firestream-ci ci: dispatching");

    // Re-invoke the binary as `firestream-ci <sub> <forward...>`. Export the
    // canonical CI_RUNNER so downstream subcommands (and any nested bash)
    // see the same value.
    std::env::set_var("CI_RUNNER", runner.label());

    let self_exe =
        std::env::current_exe().map_err(|e| firestream_ci::Error::Other(format!("current_exe: {e}")))?;
    let mut argv: Vec<String> = vec![sub.to_string()];
    // Forward the output mode. Only ci-linux consumes it today; the others
    // ignore an unknown `--output-format` is NOT safe (clap would error), so
    // restrict forwarding to the host-linux runner.
    if matches!(args.output_format, OutputFormat::Json) && matches!(runner, Runner::HostLinux) {
        argv.push("--output-format".to_string());
        argv.push("json".to_string());
    }
    argv.extend(args.forward.into_iter());

    let status = std::process::Command::new(self_exe)
        .args(&argv)
        .status()
        .map_err(|e| firestream_ci::Error::Other(format!("spawn {sub}: {e}")))?;

    Ok(u8::try_from(status.code().unwrap_or(1).clamp(0, 255)).unwrap_or(1))
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci ci-cloudbuild`
// ───────────────────────────────────────────────────────────────────────────
//
// Tiny policy bundle. Asserts BUILD_ID, sets Cloud Build resource defaults
// (`:=` semantics from the bash), turns on OTEL_REPLAY_REQUIRED, then
// delegates to `firestream-ci ci-docker` on the same binary.

/// Host-side context for the Cloud Build entrypoint. The bulk of the work
/// happens after delegation to `ci-docker`, but a typed ctx that owns the
/// rundir keeps the `RunContext` trait surface uniform across runners and
/// gives the scopeguard somewhere to anchor an abort-finalize.
#[derive(Debug)]
struct CiCloudbuildCtx {
    rundir: RunDir,
    /// Phase 4 — artifact policy. Cloud Build delegates to ci-docker which
    /// re-invokes ci-linux with these flags; the field is here so the ctx
    /// contract stays uniform across runners. Reads are forwarded through
    /// argv to the inner ci-docker invocation; `dead_code` is suppressed
    /// because the read path is via the spawned subprocess, not local code.
    #[allow(dead_code)]
    artifacts_format: ArtifactsFormat,
    #[allow(dead_code)]
    artifacts_max_bytes: u64,
    /// Phase 6 — Honeycomb opt-out. Cloud Build forwards to ci-docker which
    /// in turn forwards to ci-linux. Default `false`: Cloud Build runs
    /// already set `OTEL_REPLAY_REQUIRED=1` and gate on `HONEYCOMB_API_KEY`
    /// in the operator env, so auto-replay is the expected behaviour.
    #[allow(dead_code)]
    no_export_honeycomb: bool,
    #[allow(dead_code)]
    export_honeycomb_on_failure: bool,
}

impl RunContext for CiCloudbuildCtx {
    fn rundir(&self) -> &RunDir {
        &self.rundir
    }
}

async fn ci_cloudbuild(args: CiCloudbuildArgs) -> Result<u8, firestream_ci::Error> {
    // Hard assertion: must be inside Cloud Build. BUILD_ID is the canonical
    // signal the bash uses (line 19).
    let build_id = std::env::var("BUILD_ID").ok().unwrap_or_default();
    if build_id.is_empty() {
        eprintln!("firestream-ci ci-cloudbuild: requires Cloud Build env (BUILD_ID is unset).");
        eprintln!("Use CI_RUNNER=docker for local Docker runs.");
        return Ok(2);
    }

    // Cloud Build policy defaults — :=-style (set if unset).
    set_default_env("DOCKER_MEMORY", "30g");
    set_default_env("DOCKER_SWAP", "-1");
    set_default_env("NIX_BUILD_MAX_JOBS", "auto");
    set_default_env("NIX_BUILD_CORES", "0");
    // Honeycomb replay is mandatory on Cloud Build (line 36).
    std::env::set_var("OTEL_REPLAY_REQUIRED", "1");

    // Host-side ctx + signal/abort plumbing. Cloud Build spawns ci-docker
    // as a child; on SIGINT/SIGTERM (e.g. operator-cancelled build) the
    // scopeguard finalizes the rundir manifest with outcome=aborted before
    // the process exits.
    let ctx = build_ci_cloudbuild_ctx().await?;
    // Two-stage signal handling — first ^C cancels, second ^C force-exits.
    // ci-cloudbuild doesn't drive the pipeline directly (it spawns ci-docker
    // as a child), so we only need the abort flag for the scopeguard; the
    // notify is unused here but the second-^C escalation still fires.
    let (abort, _cancel) = install_signal_handler();
    let finalize_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = {
        let ctx_rundir = ctx.rundir.clone();
        let finalize_done = finalize_done.clone();
        let abort = abort.clone();
        scopeguard::guard((), move |_| {
            if finalize_done.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            struct Hold(RunDir);
            impl RunContext for Hold {
                fn rundir(&self) -> &RunDir {
                    &self.0
                }
            }
            let hold = Hold(ctx_rundir);
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok();
                if let Some(rt) = rt {
                    rt.block_on(async {
                        let _ = finalize_manifest(&hold, Outcome::Aborted).await;
                    });
                }
            })
            .join();
            let _ = abort;
        })
    };
    std::env::set_var("BUILD_OUTPUT_DIR", ctx.rundir.path());

    eprintln!(
        "firestream-ci ci-cloudbuild: BUILD_ID={build_id} DOCKER_MEMORY={}",
        std::env::var("DOCKER_MEMORY").unwrap_or_default()
    );

    // Delegate to ci-docker. Re-invoke this same binary so the dispatcher
    // / sub-runner separation is preserved.
    let self_exe =
        std::env::current_exe().map_err(|e| firestream_ci::Error::Other(format!("current_exe: {e}")))?;
    // Always copy artifacts out of the build container on Cloud Build: the
    // `_build` bind mount may not share back to this orchestration step, so
    // later steps (publish, deploy) need the run-dir materialized into
    // /workspace/_build explicitly.
    let mut argv: Vec<String> = vec!["ci-docker".to_string(), "--copy-out-artifacts".to_string()];
    argv.extend(args.forward.into_iter());
    let status = std::process::Command::new(self_exe)
        .args(&argv)
        .status()
        .map_err(|e| firestream_ci::Error::Other(format!("spawn ci-docker: {e}")))?;
    // Inner ci-docker has already finalized the manifest by now (success or
    // failure). Mark done so the scopeguard skips its abort-finalize.
    finalize_done.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(u8::try_from(status.code().unwrap_or(1).clamp(0, 255)).unwrap_or(1))
}

fn set_default_env(key: &str, default_val: &str) {
    if std::env::var_os(key).is_none_or(|v| v.is_empty()) {
        std::env::set_var(key, default_val);
    }
}

async fn build_ci_cloudbuild_ctx() -> Result<CiCloudbuildCtx, firestream_ci::Error> {
    // Same adopt-or-allocate shape as the docker host.
    let rundir = match std::env::var("BUILD_OUTPUT_DIR") {
        Ok(s) if !s.is_empty() => {
            let p = PathBuf::from(s);
            RunDir::open(&p)
                .await
                .map_err(|e| firestream_ci::Error::Other(format!("rundir open {}: {e}", p.display())))?
        }
        _ => allocate_run_dir().await?,
    };
    Ok(CiCloudbuildCtx {
        rundir,
        artifacts_format: ArtifactsFormat::Copy,
        artifacts_max_bytes: 2 * 1024 * 1024 * 1024,
        no_export_honeycomb: false,
        export_honeycomb_on_failure: false,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// `firestream-ci ci-docker`
// ───────────────────────────────────────────────────────────────────────────
//
// The Docker harness. The bash script's flow:
//   1. Allocate run dir + start ci-console transcript
//   2. Resolve BRANCH + builder image (multi-tier fallback)
//   3. docker create … (with passthrough env + volumes)
//   4. copy_source_to_container (tracked + untracked-but-not-ignored)
//   5. docker start -a (run ci-linux inside)
//   6. On success: commit_flatten_builder → atomic retag → push if branch
//
// The Rust version composes:
//   - the passthrough allowlist (`profile.passthrough_vars`, from ci-manifest.json)
//   - `passthrough::EnvAllowlist::snapshot_from_process` for the captured env
//   - `oci::source_sync::copy_source_to_container` for step 4
//   - `oci::flatten::commit_flatten_builder` for step 6
//
// Steps that involve git+registry walking (branch sanitization, builder
// image tier walking with branch/main/base fallback) currently live in
// `bin/_lib.sh::resolve_branch` and `::resolve_builder_image`. Rather than
// re-implementing those here, we shell out to a small bash helper that
// emits resolved values as `KEY=VAL` lines — the orchestration stays in
// Rust, the registry math stays in the proven bash. This is the strangler
// shape; M8+ can lift those into typed Rust modules incrementally.

/// Host-side context for the Docker harness. Owns the rundir so the host
/// has its own typed handle to the same physical directory the container
/// will adopt via `BUILD_OUTPUT_DIR` + `RunDir::open` + `verify_sentinel`.
#[derive(Debug)]
struct CiDockerCtx {
    rundir: RunDir,
    /// Phase 4 — artifact policy forwarded into the inner ci-linux invocation
    /// via argv. The host-side ctx doesn't consume the fields itself, but
    /// keeps them so the ctx contract is uniform across runners.
    #[allow(dead_code)]
    artifacts_format: ArtifactsFormat,
    #[allow(dead_code)]
    artifacts_max_bytes: u64,
    /// Phase 6 — Honeycomb opt-out, forwarded into the inner ci-linux via
    /// argv. Host-side ci-docker doesn't run a replay itself; the inner
    /// ci-linux is the one that owns the rundir spans and ships them.
    #[allow(dead_code)]
    no_export_honeycomb: bool,
    #[allow(dead_code)]
    export_honeycomb_on_failure: bool,
}

impl RunContext for CiDockerCtx {
    fn rundir(&self) -> &RunDir {
        &self.rundir
    }
}

async fn ci_docker(
    args: CiDockerArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::oci::{
        GitLsFiles, LineageLabels, commit_flatten_builder, copy_source_to_container,
        shared_docker_client,
    };

    // Snapshot fields needed across the function body BEFORE we start
    // moving out of `args` (clap's derive type holds non-Copy fields).
    let artifacts_format = args.artifacts_format;
    let artifacts_max_bytes = args.artifacts_max_bytes;
    let no_export_honeycomb = args.no_export_honeycomb;
    let export_honeycomb_on_failure = args.export_honeycomb_on_failure;
    let skip_flatten = args.skip_flatten;
    let skip_push = args.skip_push;
    let copy_out_artifacts = args.copy_out_artifacts;

    let mode = args
        .mode
        .or_else(|| std::env::var("CI_MODE").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".into());

    let container_arch = args
        .arch
        .or_else(|| std::env::var("CONTAINER_ARCH").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(detect_host_arch);

    // Host-side rundir allocation. If BUILD_OUTPUT_DIR is already set, the
    // operator wants a specific dir (adopt via `open`); otherwise we
    // `create` one and export the path to the container.
    let ctx = build_ci_docker_ctx_inner(
        artifacts_format,
        artifacts_max_bytes,
        no_export_honeycomb,
        export_honeycomb_on_failure,
    )
    .await?;
    // Signal handler + scopeguard mirror the ci-linux pattern. The Docker
    // host process won't write artifacts itself, but on signal-abort we
    // still want a manifest with outcome=aborted so the rundir is
    // discoverable. Second-^C force-exits via the handler.
    let (abort, _cancel) = install_signal_handler();
    let finalize_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = {
        let ctx_rundir = ctx.rundir.clone();
        let finalize_done = finalize_done.clone();
        let abort = abort.clone();
        scopeguard::guard((), move |_| {
            if finalize_done.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            struct Hold(RunDir);
            impl RunContext for Hold {
                fn rundir(&self) -> &RunDir {
                    &self.0
                }
            }
            let hold = Hold(ctx_rundir);
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok();
                if let Some(rt) = rt {
                    rt.block_on(async {
                        let _ = finalize_manifest(&hold, Outcome::Aborted).await;
                    });
                }
            })
            .join();
            let _ = abort;
        })
    };
    // Export the rundir path so the inner ci-linux can adopt it via
    // RunDir::open + verify_sentinel.
    std::env::set_var("BUILD_OUTPUT_DIR", ctx.rundir.path());

    // Step 1: resolve BRANCH and BUILDER_TAG via the existing bash helpers.
    // The bash sources `_lib.sh` and `build/_common.sh`, then runs
    // `resolve_branch` and `resolve_builder_image true`. We do the same
    // through a single helper subshell so the proven multi-tier resolution
    // stays intact during the strangler phase.
    let resolved = resolve_docker_state(&container_arch).await?;
    let branch = resolved
        .get("BRANCH")
        .cloned()
        .unwrap_or_else(|| "main".into());
    let builder_tag = resolved
        .get("BUILDER_TAG")
        .cloned()
        .ok_or_else(|| firestream_ci::Error::Other("ci-docker: BUILDER_TAG unresolved".into()))?;
    let repo_root = resolved
        .get("REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // Builder identity is profile data (`builder.image_name`, and its
    // siblings `builder.base_image` / `builder.min_image_size_bytes` /
    // `builder.container_name_prefix`). BUILDER_IMAGE_NAME still wins so an
    // operator can retarget a single run without editing the manifest.
    let image_name = std::env::var("BUILDER_IMAGE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| profile.builder.image_name.clone());
    if image_name.is_empty() {
        return Err(firestream_ci::Error::Other(
            "ci-docker: no builder image name — set BUILDER_IMAGE_NAME or \
             `builder.image_name` in the CI profile"
                .into(),
        ));
    }
    let local_tag = format!("{image_name}:{branch}-{container_arch}");
    let container_name = format!("{}{}", profile.container_name_prefix(), std::process::id());

    eprintln!("firestream-ci ci-docker: branch={branch} arch={container_arch} builder_tag={builder_tag}");
    eprintln!("firestream-ci ci-docker: container={container_name} mode={mode}");

    // Step 2: assemble allowlist + snapshot env for docker -e flags.
    let allowlist = build_passthrough_allowlist(profile)?;
    let snap = allowlist.snapshot_from_process();
    let env_args = snap.to_docker_args();

    // Step 3: docker create. The bash assembles a complex argv; we
    // delegate the create+start to the docker CLI through the same
    // resolved state, since bollard's create surface for our many flags
    // (--cgroupns=host, --memory-swap, --platform, --init, --entrypoint)
    // would require lots of typed plumbing. We compose the argv from
    // Rust state instead of bash variables.
    let mut create_argv: Vec<String> = vec![
        "create".into(),
        "--init".into(),
        "--entrypoint".into(),
        "".into(),
        "--name".into(),
        container_name.clone(),
        "--cpus".into(),
        // Default to every host core: the container's nix builds are the only
        // heavy work while CI runs, and the 2-core historical default left the
        // rest of the machine idle for the whole pipeline.
        std::env::var("DOCKER_CPUS").unwrap_or_else(|_| {
            std::thread::available_parallelism()
                .map(|n| n.get().to_string())
                .unwrap_or_else(|_| "2".into())
        }),
        "--memory".into(),
        std::env::var("DOCKER_MEMORY").unwrap_or_else(|_| "24g".into()),
        "--memory-swap".into(),
        std::env::var("DOCKER_SWAP").unwrap_or_else(|_| "-1".into()),
        "--platform".into(),
        arch_to_docker_platform(&container_arch),
        "--cgroupns=host".into(),
    ];
    match resolved
        .get("DOCKER_VOLUME_ARGS_LINE")
        .filter(|v| !v.trim().is_empty())
    {
        Some(vols) => {
            for part in shell_split(vols) {
                create_argv.push(part);
            }
        }
        None => {
            // Fallback to the profile's `build_strategy.docker_cache_volume`
            // — the persistent per-arch Nix store volume that IS the
            // docker-side warm cache. Only used when bash gave us nothing, so
            // the two can never both mount /nix.
            //
            // PHASE 7 CORRECTION. This previously expanded `{arch}` through
            // `norm_arch`, yielding `firestream-nix-store-x86_64`. The volume
            // `bin/build/strategy.sh::get_nix_volume` actually creates and
            // fills is `firestream-nix-store-amd64` (confirmed: it is the one
            // that exists in `docker volume ls` on the dev host). The old
            // expansion would have silently mounted a *different, empty*
            // volume — no error, just a cold cache and hours of rebuild.
            //
            // It also fired unconditionally in practice: no `_lib.sh` or
            // `build_docker_volume_args` exists anywhere in `bin/`, so the
            // bash helper never emits DOCKER_VOLUME_ARGS_LINE and this branch
            // is the ONLY source of the mount today.
            //
            // `platform::docker_cache_volume` takes the RAW arch and maps it
            // the way `get_nix_volume` does; it is gated by
            // `profile_docker_cache_volume_template_matches_get_nix_volume`
            // against the same golden vectors as the shell.
            let vol = firestream_ci::platform::docker_cache_volume(
                &profile.build_strategy.docker_cache_volume,
                &container_arch,
            );
            if !vol.is_empty() {
                eprintln!(
                    "firestream-ci ci-docker: no DOCKER_VOLUME_ARGS_LINE; \
                     using the profile's cache volume {vol}:/nix"
                );
                create_argv.push("-v".into());
                create_argv.push(format!("{vol}:/nix"));
            }
        }
    }
    create_argv.push("-w".into());
    create_argv.push(repo_root.display().to_string());
    let build_output_dir = std::env::var("BUILD_OUTPUT_DIR").unwrap_or_default();
    if !build_output_dir.is_empty() {
        create_argv.push("-e".into());
        create_argv.push(format!("BUILD_OUTPUT_DIR={build_output_dir}"));
    }
    create_argv.push("-e".into());
    create_argv.push("RUNNING_IN_DOCKER=1".into());
    create_argv.push("-e".into());
    create_argv.push("CI_RUNNER=docker".into());
    create_argv.push("-e".into());
    create_argv.push(format!("CI_MODE={mode}"));
    create_argv.push("-e".into());
    create_argv.push("SKIP_DOCKER_LOAD=1".into());
    create_argv.push("-e".into());
    create_argv.push(format!("HOST_UID={}", current_uid()));
    create_argv.push("-e".into());
    create_argv.push(format!("HOST_GID={}", current_gid()));
    // Allowlisted env via passthrough module (BTreeMap order → deterministic).
    create_argv.extend(env_args);
    create_argv.push(builder_tag.clone());
    // Inner command: invoke `firestream-ci ci-linux` directly. The bash shim
    // `bin/ci/ci-linux.sh` was retired post-M7; the dev-env put firestream-ci on
    // PATH inside the builder image.
    //
    // Append the artifact materialisation flags so the inner ci-linux
    // honours the host-side knob. The DEVSHELL_INIT_SNIPPET helper emits
    // `... firestream-ci ci-linux` (or a `nix develop ... -c bash -c '... firestream-ci
    // ci-linux'` wrapper); appending the flags as plain argv works in
    // either shape because the snippet treats the trailing tokens as the
    // command to run.
    let inner_base = resolved
        .get("DEVSHELL_INIT_SNIPPET")
        .cloned()
        .unwrap_or_else(|| "firestream-ci ci-linux".to_string());
    let mut inner = format!(
        "{inner_base} --artifacts-format={fmt} --artifacts-max-bytes={max}",
        fmt = artifacts_format.as_str(),
        max = artifacts_max_bytes,
    );
    if no_export_honeycomb {
        inner.push_str(" --no-export-honeycomb");
    }
    if export_honeycomb_on_failure {
        inner.push_str(" --export-honeycomb-on-failure");
    }
    create_argv.push("bash".into());
    create_argv.push("-c".into());
    create_argv.push(inner);

    // Step 3 exec: docker create.
    let create_status = std::process::Command::new("docker")
        .args(&create_argv)
        .status()
        .map_err(|e| firestream_ci::Error::Other(format!("docker create: {e}")))?;
    if !create_status.success() {
        eprintln!("firestream-ci ci-docker: docker create failed — argv:");
        for (i, a) in create_argv.iter().enumerate() {
            eprintln!("  {i:3}: {a:?}");
        }
        return Ok(u8::try_from(create_status.code().unwrap_or(1).clamp(0, 255)).unwrap_or(1));
    }

    // Step 4: copy source via the library (bollard upload_to_container).
    let docker_client = shared_docker_client()?;
    let sources = GitLsFiles::collect(&repo_root)
        .await
        .map_err(firestream_ci::Error::from)?;
    let bytes =
        copy_source_to_container(&docker_client, &container_name, &repo_root, "/", &sources)
            .await
            .map_err(firestream_ci::Error::from)?;
    eprintln!("firestream-ci ci-docker: copied {bytes} bytes of source");

    // Step 5: docker start -a — run the inner CI.
    let start_status = std::process::Command::new("docker")
        .args(["start", "-a", &container_name])
        .status()
        .map_err(|e| firestream_ci::Error::Other(format!("docker start: {e}")))?;
    let ci_exit = start_status.code().unwrap_or(1);

    // Step 5b: copy artifacts out of the build container (Cloud Build).
    // The build writes the run-dir under the `_build` bind mount, which on a
    // dev host shares straight back. On Cloud Build the orchestration step is
    // itself a container and the bind-mount source may resolve to a different
    // path on the worker VM, so the run-dir can be invisible here. `docker cp`
    // the container's run-dir into the host BUILD_OUTPUT_DIR so downstream
    // steps (publish, deploy) see the artifacts + manifest. Best-effort: the
    // bind mount may already have delivered them, so a failure is non-fatal.
    if ci_exit == 0 && copy_out_artifacts && !build_output_dir.is_empty() {
        if let Err(e) = std::fs::create_dir_all(&build_output_dir) {
            eprintln!(
                "firestream-ci ci-docker: copy-out mkdir {build_output_dir} failed (non-fatal): {e}"
            );
        }
        let src = format!("{container_name}:{build_output_dir}/.");
        eprintln!("firestream-ci ci-docker: copying artifacts {src} -> {build_output_dir}");
        let cp = std::process::Command::new("docker")
            .args(["cp", &src, &build_output_dir])
            .status();
        match cp {
            Ok(s) if s.success() => eprintln!("firestream-ci ci-docker: artifact copy-out ok"),
            Ok(s) => eprintln!(
                "firestream-ci ci-docker: artifact copy-out exited {} (non-fatal)",
                s.code().unwrap_or(-1)
            ),
            Err(e) => eprintln!("firestream-ci ci-docker: artifact copy-out failed (non-fatal): {e}"),
        }
    }

    // Step 6: warm-cache flatten on success.
    let mut flatten_status_label = "skipped";
    if ci_exit == 0 && !skip_flatten {
        let labels = LineageLabels {
            branch: branch.clone(),
            sha: resolved.get("GIT_SHA").cloned().unwrap_or_default(),
            epoch: epoch_secs().to_string(),
            parent_sha: resolved.get("PARENT_SHA").cloned().unwrap_or_default(),
            trace_id: std::env::var("TRACEPARENT").unwrap_or_default(),
            arch: container_arch.clone(),
            flatten_ts: epoch_secs().to_string(),
        };
        let known_arches = ["x86_64", "aarch64"];
        match commit_flatten_builder(
            &docker_client,
            &container_name,
            &local_tag,
            Some(&builder_tag),
            labels,
            &image_name,
            &container_arch,
            &known_arches,
            profile.builder.min_image_size_bytes,
            &profile.container_name_prefix(),
        )
        .await
        {
            Ok(res) => {
                eprintln!(
                    "firestream-ci ci-docker: flatten ok tag={} size={} reaped={}",
                    res.tag, res.size_bytes, res.reaped_count
                );
                flatten_status_label = "ok";
                // Optional registry push (main/nightly only — bash policy).
                let registry = std::env::var("AR_REGISTRY").unwrap_or_default();
                if !skip_push && !registry.is_empty() && (branch == "main" || branch == "nightly") {
                    let remote = format!("{registry}/{image_name}:{branch}-{container_arch}");
                    eprintln!("firestream-ci ci-docker: pushing {remote}");
                    let tag_ok = std::process::Command::new("docker")
                        .args(["tag", &local_tag, &remote])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if tag_ok {
                        let _ = std::process::Command::new("docker")
                            .args(["push", &remote])
                            .status();
                    }
                }
            }
            Err(e) => {
                eprintln!("firestream-ci ci-docker: flatten failed (non-fatal): {e}");
                flatten_status_label = "error";
            }
        }
    } else if ci_exit != 0 {
        eprintln!("firestream-ci ci-docker: CI failed (exit {ci_exit}) — skipping warm-cache commit");
    }

    // Cleanup.
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &container_name])
        .status();

    // The inner ci-linux already wrote its own manifest with the run
    // outcome — don't double-write from the host on the normal exit path.
    // Flip the finalize-done flag so the scopeguard treats this as a
    // clean shutdown.
    finalize_done.store(true, std::sync::atomic::Ordering::SeqCst);

    if ci_exit != 0 {
        return Ok(u8::try_from(ci_exit.clamp(0, 255)).unwrap_or(1));
    }
    eprintln!("firestream-ci ci-docker: passed (warm cache: {flatten_status_label})");
    Ok(0)
}

async fn build_ci_docker_ctx_inner(
    artifacts_format: ArtifactsFormat,
    artifacts_max_bytes: u64,
    no_export_honeycomb: bool,
    export_honeycomb_on_failure: bool,
) -> Result<CiDockerCtx, firestream_ci::Error> {
    // Adopt-or-allocate. If BUILD_OUTPUT_DIR points somewhere, the operator
    // controls the path; otherwise allocate a fresh rundir via the canonical
    // RunDir::create contract. Either way the host writes the sentinel so
    // the container-side `verify_sentinel()` can confirm the volume mount.
    let rundir = match std::env::var("BUILD_OUTPUT_DIR") {
        Ok(s) if !s.is_empty() => {
            let p = PathBuf::from(s);
            // `create` is idempotent at the subdir level (and writes the
            // sentinel) — use it rather than `open` so a pre-existing path
            // gets a valid sentinel anchored at the host view.
            // But `create` allocates a new <date>/<sha>/<epoch> — so only
            // allocate-fresh here, otherwise just `open`.
            RunDir::open(&p)
                .await
                .map_err(|e| firestream_ci::Error::Other(format!("rundir open {}: {e}", p.display())))?
        }
        _ => allocate_run_dir().await?,
    };
    Ok(CiDockerCtx {
        rundir,
        artifacts_format,
        artifacts_max_bytes,
        no_export_honeycomb,
        export_honeycomb_on_failure,
    })
}

/// Shell out to a small bash helper that sources `bin/_lib.sh` + `bin/build/_common.sh`,
/// calls `resolve_branch` and `resolve_builder_image true`, and prints the
/// resulting state as `KEY=VAL` lines. The proven multi-tier resolution
/// and per-arch tag logic stay in the bash; this binary just consumes
/// their output.
async fn resolve_docker_state(
    container_arch: &str,
) -> Result<std::collections::HashMap<String, String>, firestream_ci::Error> {
    let script = r#"
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Caller arranges its cwd inside the repo root.
. bin/_lib.sh
. bin/build/_common.sh
resolve_branch >/dev/null 2>&1 || true
resolve_builder_image true >/dev/null 2>&1 || true
build_docker_volume_args true >/dev/null 2>&1 || true
GIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
PARENT_SHA="$(git rev-parse HEAD^ 2>/dev/null || echo)"
DEVSHELL_INIT_SNIPPET="$(devshell_init_snippet 'firestream-ci ci-linux' true 2>/dev/null || echo 'firestream-ci ci-linux')"
DOCKER_VOLUME_ARGS_LINE=""
if [[ -n "${DOCKER_VOLUME_ARGS+x}" ]]; then
    DOCKER_VOLUME_ARGS_LINE="${DOCKER_VOLUME_ARGS[*]}"
fi
echo "BRANCH=${BRANCH:-}"
echo "BUILDER_TAG=${BUILDER_TAG:-}"
echo "REPO_ROOT=${REPO_ROOT:-$PWD}"
echo "GIT_SHA=${GIT_SHA}"
echo "PARENT_SHA=${PARENT_SHA}"
echo "DOCKER_VOLUME_ARGS_LINE=${DOCKER_VOLUME_ARGS_LINE}"
echo "DEVSHELL_INIT_SNIPPET=${DEVSHELL_INIT_SNIPPET}"
"#;
    let output = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("CONTAINER_ARCH", container_arch)
        .output()
        .await
        .map_err(|e| firestream_ci::Error::Other(format!("resolve helper: spawn: {e}")))?;
    if !output.status.success() {
        return Err(firestream_ci::Error::Other(format!(
            "resolve helper exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut map = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    Ok(map)
}

/// The host→builder env allowlist, straight from `profile.passthrough_vars`.
///
/// `FIRESTREAM_CI_PROFILE` is appended unconditionally: the inner `ci-linux`
/// running inside the builder must resolve the same manifest as its parent, and
/// a profile that forgot to list its own locator would silently give the child
/// the built-in default. This is not project knowledge — it is this tool's own
/// env var, the way `passthrough` already treats `TRACEPARENT`.
fn build_passthrough_allowlist(
    profile: &firestream_ci::Profile,
) -> Result<firestream_ci::passthrough::EnvAllowlist, firestream_ci::Error> {
    use firestream_ci::passthrough::EnvAllowlist;
    let mut vars = profile.passthrough_vars.clone();
    let self_locator = firestream_ci::profile::PROFILE_ENV.to_string();
    if !vars.contains(&self_locator) {
        vars.push(self_locator);
    }
    Ok(EnvAllowlist::builder().add_all(vars)?.build())
}

fn arch_to_docker_platform(arch: &str) -> String {
    match arch {
        "x86_64" | "amd64" => "linux/amd64".into(),
        "aarch64" | "arm64" => "linux/arm64".into(),
        other => format!("linux/{other}"),
    }
}

fn shell_split(s: &str) -> Vec<String> {
    // Simple whitespace split — the bash array stringification produces
    // space-separated tokens for well-formed volume args.
    s.split_whitespace().map(|t| t.to_string()).collect()
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // Shell out to `id -u`. Avoids pulling in libc for a single syscall.
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}
#[cfg(unix)]
fn current_gid() -> u32 {
    std::process::Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}
#[cfg(not(unix))]
fn current_gid() -> u32 {
    0
}

// ───────────────────────────────────────────────────────────────────────────
// `ci-darwin` — REMOVED IN THE FIRESTREAM LIFT
// ───────────────────────────────────────────────────────────────────────────
//
// ConceptDB's `ci-darwin` body was an Xcode/xcodegen/xcodebuild iOS +
// macOS-desktop pipeline. Firestream has no Apple app targets, so the body
// (`CiDarwinArgs`, `CiDarwinCtx`, `ci_darwin`, the prechecks/desktop/iOS
// tasks, the codesign lock and the Darwin watchdog sidecar) was deleted
// rather than carried as dead weight.
//
// The Darwin *runner shell* is deliberately KEPT: `Runner::HostDarwin` and
// its detection in `firestream_ci::runner` still exist, because building
// Linux images from a macOS host is exactly what `nix/flake-modules/
// docker-build.nix` serves. What is missing is a Firestream-shaped body for
// that runner (a rewrite, not an adaptation — see the plan's Risks table).
// Until then `firestream-ci ci` dispatching on a macOS host reports that the
// runner has no implementation instead of silently doing nothing.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn format_task_failure_tails_log_and_adds_fmt_hint() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("verify-required-rust-fmt.stderr.log");
        let fake_diff = (1..=25)
            .map(|i| format!("line {i}: Diff in src/foo.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&log, &fake_diff).unwrap();

        let out = format_task_failure(
            "build failed (exit 1, 1 failed entries)",
            &log,
            "required-rust-fmt",
        );

        assert!(out.starts_with("build failed (exit 1, 1 failed entries)"));
        assert!(out.contains(&log.display().to_string()));
        // Only the last 12 signal lines are kept; earlier lines are trimmed.
        assert!(!out.contains("line 1: "));
        assert!(out.contains("line 25: "));
        assert!(out.contains("hint: run `cargo fmt`"));
    }

    #[test]
    fn read_log_tail_strips_ansi_and_progress_noise() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("nix.stderr.log");
        // A realistic mix: colorized build chatter that should be dropped,
        // then the real failure line that must survive.
        std::fs::write(
            &log,
            concat!(
                "\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m: core-types: Copying /out/a.rs\n",
                "\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m: core-types: Copying /out/b.rs\n",
                "    Finished `dev` profile in 7m\n",
                "Running phase: installPhase\n",
                "\u{1b}[31mmkdir: cannot create directory: File exists\u{1b}[0m\n",
            ),
        )
        .unwrap();

        let tail = read_log_tail(&log, 12).unwrap();
        // ANSI is gone, not echoed literally.
        assert!(!tail.contains('\u{1b}'), "escape survived: {tail:?}");
        assert!(!tail.contains("[33m"), "literal SGR survived: {tail:?}");
        // Progress chatter is filtered out, so the real error is the tail.
        assert!(!tail.contains("Copying"));
        assert!(!tail.contains("Finished"));
        assert_eq!(tail, "mkdir: cannot create directory: File exists");
    }

    #[test]
    fn strip_ansi_removes_csi_keeps_text() {
        assert_eq!(strip_ansi("\u{1b}[1;31mFAIL\u{1b}[0m here"), "FAIL here");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn format_task_failure_handles_missing_log() {
        let out = format_task_failure(
            "build failed (exit 1, 1 failed entries)",
            std::path::Path::new("/nonexistent/log"),
            "required-rust-clippy",
        );
        assert_eq!(out, "build failed (exit 1, 1 failed entries)");
        // No hint for clippy: we only emit hints we're sure about.
    }

    #[test]
    fn leaf_hint_only_fires_for_known_leaves() {
        assert_eq!(
            leaf_hint("required-rust-fmt"),
            Some("hint: run `cargo fmt` to fix formatting")
        );
        assert_eq!(leaf_hint("required-rust-clippy"), None);
        assert_eq!(leaf_hint("required-ts-vitest"), None);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// build images / build manifest / build resolve
//
// The typed half of the Phase-7 strangler. `bin/build/container-images.sh` and
// `bin/build/manifest.sh` remain the default; these run only when the caller
// opts in with FIRESTREAM_BUILD_IMPL=rust / --rust / make IMPL=rust.
// ───────────────────────────────────────────────────────────────────────────

/// Line-by-line mirror of `container-images.sh`'s argument loop, extended with
/// the four flags that only exist on this path.
///
/// The `--version` latch is the load-bearing part: it applies to the container
/// that comes AFTER it, and a trailing `--version` is consumed by nothing.
/// That is not a typo in this port — it is what the shell does, and
/// `makefile:544` (`redis-build-%: $(BUILD_CONTAINER) redis --version $*`)
/// depends on it in the wrong direction. See [`ParsedBuildArgv::dangling_version`].
fn parse_images_argv(argv: &[String]) -> Result<ParsedBuildArgv, firestream_ci::Error> {
    let mut out = ParsedBuildArgv::default();
    let mut current_version = String::new();
    let mut i = 0usize;

    let need = |i: usize, flag: &str, argv: &[String]| -> Result<String, firestream_ci::Error> {
        argv.get(i + 1)
            .cloned()
            .ok_or_else(|| firestream_ci::Error::Other(format!("{flag} requires argument")))
    };

    while i < argv.len() {
        match argv[i].as_str() {
            "--version" => {
                current_version = need(i, "--version", argv)?;
                i += 2;
            }
            "--target" => {
                out.target = Some(need(i, "--target", argv)?);
                i += 2;
            }
            "--repo-root" => {
                out.repo_root = Some(PathBuf::from(need(i, "--repo-root", argv)?));
                i += 2;
            }
            "--build-output-dir" => {
                out.build_output_dir = Some(PathBuf::from(need(i, "--build-output-dir", argv)?));
                i += 2;
            }
            "--native" => {
                out.strategy = Some("native");
                i += 1;
            }
            "--docker" => {
                out.strategy = Some("docker");
                i += 1;
            }
            "--no-load" => {
                out.no_load = true;
                i += 1;
            }
            "--dry-run" | "-n" => {
                out.dry_run = true;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(firestream_ci::Error::Other(format!(
                    "Unknown option: {other}"
                )));
            }
            other => {
                out.containers
                    .push((other.to_string(), std::mem::take(&mut current_version)));
                i += 1;
            }
        }
    }

    if !current_version.is_empty() {
        out.dangling_version = Some(current_version);
    }
    Ok(out)
}

/// Erroring wrapper over [`firestream_ci::rundir::find_repo_root`] (itself the
/// mirror of `_common.sh::find_repo_root`). The `images`/`manifest` paths
/// cannot proceed without a repo root, so absence is a hard error here; the
/// rundir-allocating paths degrade to cwd instead.
fn find_repo_root(start: &Path) -> Result<PathBuf, firestream_ci::Error> {
    firestream_ci::rundir::find_repo_root(start).ok_or_else(|| {
        firestream_ci::Error::Other(
            "Could not find repo root (no flake.nix + src/containers/firestream above cwd)".into(),
        )
    })
}

/// `TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"`, the first line of both scripts.
/// An explicit `--target` wins, exactly as the shell's later `--target` branch
/// overwrites the env-seeded value.
fn resolve_target_arch(explicit: Option<&str>) -> String {
    if let Some(t) = explicit.filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    std::env::var("TARGET_ARCH")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(detect_host_arch)
}

fn build_output_dir_for(repo_root: &Path, explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| {
            std::env::var("BUILD_OUTPUT_DIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| repo_root.join("_build"))
}

/// Assemble the [`PlanEnv`] from the profile + the live host. Kept in one
/// place so `images` and `manifest` cannot drift.
fn plan_env_for(
    profile: &firestream_ci::Profile,
    target_arch: &str,
) -> (firestream_ci::imagebuild::PlanEnv, firestream_ci::platform::Decision) {
    use firestream_ci::imagebuild::{DockerResources, PlanEnv};
    let decision = firestream_ci::platform::decide_with_profile(Some(target_arch), profile);
    let strategy = decision.strategy;
    let resources = if strategy == firestream_ci::platform::BuildStrategy::Docker {
        // Lazy, exactly like fs_docker_resources: `docker info` is two
        // round-trips the native path must never pay.
        DockerResources::probe()
    } else {
        DockerResources::default()
    };
    (
        PlanEnv {
            strategy,
            resources,
            docker_cache_volume_template: profile.build_strategy.docker_cache_volume.clone(),
            builder_tag: String::new(),
        },
        decision,
    )
}

fn report_decision(d: &firestream_ci::platform::Decision) {
    use firestream_ci::platform::DecisionSource;
    for w in &d.warnings {
        eprintln!("  ! {w}");
    }
    let why = match d.source {
        DecisionSource::EnvOverride => "forced by FIRESTREAM_BUILD_STRATEGY".to_string(),
        DecisionSource::ProfileDefault => "forced by the CI profile".to_string(),
        DecisionSource::Probed => match &d.blocker {
            Some(b) => format!("falling back to the Docker builder: {b}"),
            None => "probed: native is possible".to_string(),
        },
    };
    eprintln!("  -> Strategy: {} ({why})", d.strategy.label());
}

async fn build_images(
    args: BuildImagesArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::imagebuild::{
        self, BatchLock, BuildSpec, Output, PackageResult, Stopwatch,
    };

    let parsed = parse_images_argv(&args.argv)?;
    if parsed.containers.is_empty() {
        eprintln!(
            "usage: firestream-ci build images [--target <arch>] [--version <v>] <container>...\n\
             \n\
             known containers: {}",
            if profile.container_registry.is_empty() {
                "(none — the CI profile carries no container_registry)".to_string()
            } else {
                profile.known_containers().join(" ")
            }
        );
        return Ok(1);
    }

    if let Some(v) = &parsed.dangling_version {
        eprintln!(
            "  ! `--version {v}` came after the last container and applies to nothing — \
             the container(s) will build at their DEFAULT version. This mirrors \
             container-images.sh exactly; note `make redis-build-8` has this shape, \
             so it builds redis-7."
        );
    }

    // `--native` / `--docker` are expressed as the env var so exactly one
    // predicate (firestream_ci::platform, mirrored by strategy.sh) decides.
    if let Some(s) = parsed.strategy {
        std::env::set_var(firestream_ci::platform::ENV_BUILD_STRATEGY, s);
    }

    let repo_root = match parsed.repo_root.clone() {
        Some(r) => r,
        None => find_repo_root(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))?,
    };
    let containers_dir = repo_root.join("src/containers/firestream");
    let build_output_dir = build_output_dir_for(&repo_root, parsed.build_output_dir.clone());
    let target_arch = resolve_target_arch(parsed.target.as_deref());

    // Validate + resolve everything BEFORE taking the lock or building
    // anything, same order as the shell.
    let mut packages: Vec<(String, String)> = Vec::new(); // (pkg, container)
    for (container, version) in &parsed.containers {
        if !containers_dir.join(container).is_dir() {
            eprintln!(
                "  x Container '{container}' not found in {}",
                containers_dir.display()
            );
            return Ok(1);
        }
        let pkg = profile.resolve_package_name(container, version)?;
        packages.push((pkg, container.clone()));
    }

    eprintln!(
        ">>> Building: {} (arch: {target_arch})",
        packages
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let (env, decision) = plan_env_for(profile, &target_arch);
    report_decision(&decision);

    if parsed.dry_run {
        println!("# firestream-ci build images --dry-run");
        println!("# repo_root        {}", repo_root.display());
        println!("# build_output_dir {}", build_output_dir.display());
        println!("# target_arch      {target_arch}");
        println!("# strategy         {}", env.strategy.label());
        for (pkg, container) in &packages {
            let dest = imagebuild::image_dest(&build_output_dir, pkg);
            let spec = BuildSpec {
                flake_dir: repo_root.clone(),
                flake_ref: format!(".#{pkg}"),
                dest: dest.clone(),
                target_arch: target_arch.clone(),
                output: Output::Tarball,
                docker_sock: true,
            };
            let plan = imagebuild::plan(&spec, &env)?;
            println!("\n# {container} -> {pkg}");
            for (k, v) in imagebuild::plan_facts(&plan) {
                println!("#   {k:<12} {v}");
            }
            println!("{}", plan.render());
            println!(
                "docker load < {}    # tag extracted from `Loaded image:`",
                dest.display()
            );
        }
        return Ok(0);
    }

    std::fs::create_dir_all(&build_output_dir)
        .map_err(|e| firestream_ci::Error::Other(format!("mkdir _build: {e}")))?;
    // Same lock path as the bash, so an opted-in Rust build and a default
    // bash build cannot race over `_build/`.
    let _lock = BatchLock::acquire(&build_output_dir)?;

    let sw = Stopwatch::start();
    let mut results: Vec<PackageResult> = Vec::new();
    let total = packages.len();

    for (idx, (pkg, container)) in packages.iter().enumerate() {
        let out_dir = build_output_dir.join(pkg);
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| firestream_ci::Error::Other(format!("mkdir {}: {e}", out_dir.display())))?;
        let dest = imagebuild::image_dest(&build_output_dir, pkg);
        let log = out_dir.join("build.log");

        eprintln!("\n>>> Building {pkg} ({}/{total})...", idx + 1);

        let spec = BuildSpec {
            flake_dir: repo_root.clone(),
            flake_ref: format!(".#{pkg}"),
            dest: dest.clone(),
            target_arch: target_arch.clone(),
            output: Output::Tarball,
            docker_sock: true,
        };
        let plan = imagebuild::plan(&spec, &env)?;

        let code = tokio::select! {
            r = imagebuild::execute(&plan, Some(&log)) => r?,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n  ! Interrupted - stopping builds...");
                return Ok(130);
            }
        };

        // Defense in depth, same as the shell: a zero exit with no output file
        // is a failure.
        let empty = std::fs::metadata(&dest).map(|m| m.len() == 0).unwrap_or(true);
        if code != 0 || empty {
            if code == 0 {
                eprintln!(
                    "  x Build reported success but output file missing or empty: {}",
                    dest.display()
                );
            }
            eprintln!("  x FAILED: {pkg} (see {})", log.display());
            results.push(PackageResult {
                package: pkg.clone(),
                container: container.clone(),
                succeeded: false,
                image_tag: None,
                note: Some(format!("exit {code}")),
            });
            continue;
        }

        // Log is only kept on failure — same as the shell.
        let _ = std::fs::remove_file(&log);

        if parsed.no_load {
            eprintln!("  v {pkg} -> {} (not loaded: --no-load)", dest.display());
            results.push(PackageResult {
                package: pkg.clone(),
                container: container.clone(),
                succeeded: true,
                image_tag: None,
                note: Some("skipped docker load".into()),
            });
            continue;
        }

        eprintln!("  -> Loading {pkg} into Docker...");
        match imagebuild::docker_load(&dest).await {
            Ok(tag) => {
                eprintln!("  v {pkg} -> {tag}");
                results.push(PackageResult {
                    package: pkg.clone(),
                    container: container.clone(),
                    succeeded: true,
                    image_tag: Some(tag),
                    note: None,
                });
            }
            Err(e) => {
                eprintln!("  x Failed to load {pkg} into Docker: {e}");
                results.push(PackageResult {
                    package: pkg.clone(),
                    container: container.clone(),
                    succeeded: false,
                    image_tag: None,
                    note: Some(e.to_string()),
                });
            }
        }
    }

    eprintln!();
    eprint!("{}", imagebuild::render_summary(&results, sw.secs()));

    let failed: Vec<&str> = results
        .iter()
        .filter(|r| !r.succeeded)
        .map(|r| r.package.as_str())
        .collect();
    if failed.is_empty() {
        Ok(0)
    } else {
        eprintln!("  x Failed containers: {}", failed.join(" "));
        Ok(1)
    }
}

async fn build_manifest(
    args: BuildManifestArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    use firestream_ci::imagebuild::{self, BuildSpec, Output, Stopwatch};

    let parsed = parse_images_argv(&args.argv)?;
    if parsed.containers.len() > 1 {
        return Err(firestream_ci::Error::Other(
            "build manifest takes at most one container".into(),
        ));
    }
    let container = parsed.containers.first().map(|(c, _)| c.clone());

    if let Some(s) = parsed.strategy {
        std::env::set_var(firestream_ci::platform::ENV_BUILD_STRATEGY, s);
    }

    let repo_root = match parsed.repo_root.clone() {
        Some(r) => r,
        None => find_repo_root(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))?,
    };
    let build_output_dir = build_output_dir_for(&repo_root, parsed.build_output_dir.clone());
    let target_arch = resolve_target_arch(parsed.target.as_deref());

    let (dest, flake_ref) = imagebuild::manifest_dest(&build_output_dir, container.as_deref());
    match &container {
        None => eprintln!(">>> Building fleet manifest"),
        Some(c) => eprintln!(">>> Building SBOM for: {c}"),
    }

    let (env, decision) = plan_env_for(profile, &target_arch);
    report_decision(&decision);

    let spec = BuildSpec {
        flake_dir: repo_root.clone(),
        flake_ref,
        dest: dest.clone(),
        target_arch: target_arch.clone(),
        // NOTE: manifest.sh passes --dir and NOT --sock. Preserved.
        output: Output::Dir,
        docker_sock: false,
    };

    // The docker plan canonicalises dirname(dest); create it first so a
    // dry run on a clean tree renders the same argv a real run would use.
    std::fs::create_dir_all(&build_output_dir)
        .map_err(|e| firestream_ci::Error::Other(format!("mkdir _build: {e}")))?;

    let plan = imagebuild::plan(&spec, &env)?;

    if parsed.dry_run {
        println!("# firestream-ci build manifest --dry-run");
        println!("# repo_root        {}", repo_root.display());
        println!("# output           {}", dest.display());
        for (k, v) in imagebuild::plan_facts(&plan) {
            println!("#   {k:<12} {v}");
        }
        println!("{}", plan.render());
        return Ok(0);
    }

    let sw = Stopwatch::start();
    let code = imagebuild::execute(&plan, None).await?;
    if code != 0 {
        eprintln!("  x Build failed after {}s", sw.secs());
        return Ok(1);
    }
    eprintln!("\n  v Build completed in {}s", sw.secs());
    eprintln!("  -> Output: {}", dest.display());
    if let Ok(rd) = std::fs::read_dir(&dest) {
        for e in rd.flatten() {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            eprintln!("     {size:>12}  {}", e.file_name().to_string_lossy());
        }
    }
    let _ = profile;
    Ok(0)
}

fn build_resolve(
    args: BuildResolveArgs,
    profile: &firestream_ci::Profile,
) -> Result<u8, firestream_ci::Error> {
    let version = args.version.unwrap_or_default();
    println!("{}", profile.resolve_package_name(&args.container, &version)?);
    Ok(0)
}
