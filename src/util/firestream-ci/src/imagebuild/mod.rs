//! Typed equivalent of `bin/build/container-images.sh` + `bin/build/manifest.sh`.
//!
//! # Status: strangler, not replacement
//!
//! The bash scripts are still the default and still work. This module is the
//! opt-in second implementation (`FIRESTREAM_BUILD_IMPL=rust`, or
//! `--rust` on either script) that has to earn the right to replace them by
//! being run on both a Linux and a Darwin host first. Where it cannot yet do
//! what the bash does, it says so out loud rather than degrading — see
//! [`Plan`]'s `--dry-run` rendering and the module-level gap notes below.
//!
//! # What is mirrored, and from where
//!
//! | Behaviour | Shell source |
//! |---|---|
//! | native `nix build --out-link` | `strategy.sh::fs_nix_build_native` |
//! | docker builder + `/nix` volume | `strategy.sh::fs_nix_build_docker` |
//! | strategy dispatch | `strategy.sh::fs_build_image` (via [`crate::platform`]) |
//! | git-worktree bind mounts | `strategy.sh::resolve_git_mounts` |
//! | docker cpu/mem/swap probes | `_common.sh::fs_docker_resources` |
//! | batch lock, per-pkg log, `docker load`, summary | `container-images.sh` |
//! | `_build/<pkg>/<pkg>.tar.gz` layout | `container-images.sh` |
//!
//! `strategy.sh` itself is untouched and stays the flake-app path: it is
//! sourced from a `/nix/store` path by `nix/flake-modules/docker-build.nix`,
//! where no Rust workspace exists.
//!
//! # The plan/execute split
//!
//! [`plan`] is **pure** — it takes a [`BuildSpec`] plus already-probed inputs
//! and returns the exact argv that would run. That is what makes this
//! verifiable without a cold multi-hour build: `--dry-run` prints the argv and
//! it can be diffed against what the shell would have done, and the unit tests
//! assert the mount chain and flag set with no Docker and no Nix.
//!
//! # Known gaps vs. the bash (deliberate, documented, NOT silent)
//!
//! * **Git-mount algorithm.** [`shell_git_mounts`] mirrors
//!   `resolve_git_mounts` exactly: `.git`-file parse, `commondir`, two `:ro`
//!   mounts, worktrees only. [`crate::worktree`] has a strictly richer walk
//!   (workdir, submodule `modules/*`, symlink divergence). Adopting it here
//!   would be a behaviour CHANGE, not a port, so it is not done in this phase.
//! * **Interrupt handling.** The bash traps INT/TERM, kills the in-flight
//!   build and skips the rest of the batch. Here Ctrl-C is observed between
//!   packages and cancels the remainder; the in-flight child is killed by the
//!   tokio runtime dropping it, which is not identical to the bash's explicit
//!   `kill` + `wait` of the process group.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use thiserror::Error;

use crate::platform::{self, BuildStrategy};

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("imagebuild: {0}")]
    Other(String),

    #[error("imagebuild: I/O on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "imagebuild: another build is running (PID {pid}). If stale, remove: rm -rf {lock}"
    )]
    LockHeld { pid: String, lock: PathBuf },

    #[error("imagebuild: docker is required for the 'docker' build strategy but was not found")]
    NoDocker,

    #[error("imagebuild: build of `{attr}` failed (exit {code}); see {log}")]
    BuildFailed {
        attr: String,
        code: i32,
        log: PathBuf,
    },
}

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> BuildError {
    let path = path.into();
    move |source| BuildError::Io { path, source }
}

// ───────────────────────────────────────────────────────────────────────────
// Spec
// ───────────────────────────────────────────────────────────────────────────

/// Shape of the Nix output. Mirrors the `--dir` flag on `fs_build_image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// A single file (the image tarball). `dest` is that file.
    Tarball,
    /// A directory of files (manifest / SBOM). `dest` is that directory.
    Dir,
}

/// One `fs_build_image` invocation, in typed form.
#[derive(Debug, Clone)]
pub struct BuildSpec {
    /// `<flake_dir>` — the repo root, or a `/nix/store` snapshot.
    pub flake_dir: PathBuf,
    /// `<flake_ref>` — e.g. `.#redis-7`.
    pub flake_ref: String,
    /// `<dest>` — out-link (native) or copy target (docker).
    pub dest: PathBuf,
    /// `<target_arch>` — RAW, as the user typed it or `uname -m` gave it.
    /// Never pre-normalised: see [`platform::docker_arch_alias`].
    pub target_arch: String,
    pub output: Output,
    /// `--sock`: bind-mount `/var/run/docker.sock` into the builder.
    pub docker_sock: bool,
}

/// Docker resource caps. Mirrors `_common.sh::fs_docker_resources` —
/// **lazily** probed, because `docker info` is a multi-second stall plus noise
/// on a host with no daemon and the native path never needs it.
#[derive(Debug, Clone, Default)]
pub struct DockerResources {
    pub cpus: Option<String>,
    pub memory: Option<String>,
    pub swap: Option<String>,
}

impl DockerResources {
    /// Env first (`DOCKER_CPUS` / `DOCKER_MEMORY` / `DOCKER_SWAP`), then
    /// `docker info`, then the bash fallbacks (`nproc`/4 CPUs, `8g` memory,
    /// swap = 2× memory).
    pub fn probe() -> Self {
        let cpus = std::env::var("DOCKER_CPUS")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| docker_info_field("{{.NCPU}}"))
            .or_else(|| {
                std::thread::available_parallelism()
                    .ok()
                    .map(|n| n.get().to_string())
            })
            .or_else(|| Some("4".to_string()));

        let memory = std::env::var("DOCKER_MEMORY")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                let bytes: u64 = docker_info_field("{{.MemTotal}}")?.trim().parse().ok()?;
                if bytes == 0 {
                    return None;
                }
                let gb = bytes / 1_073_741_824;
                // Reserve 1 GiB overhead, exactly as _detect_docker_memory.
                let usable = if gb > 1 { gb - 1 } else { 1 };
                Some(format!("{usable}g"))
            })
            .or_else(|| Some("8g".to_string()));

        let swap = std::env::var("DOCKER_SWAP")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                let m = memory.as_deref()?;
                let n: u64 = m.trim_end_matches('g').parse().ok()?;
                Some(format!("{}g", n * 2))
            });

        Self {
            cpus,
            memory,
            swap,
        }
    }
}

fn docker_info_field(fmt: &str) -> Option<String> {
    let out = std::process::Command::new("docker")
        .args(["info", "--format", fmt])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

// ───────────────────────────────────────────────────────────────────────────
// Git mounts — verbatim mirror of strategy.sh::resolve_git_mounts
// ───────────────────────────────────────────────────────────────────────────

/// The two `:ro` bind mounts a git **worktree** needs so that the absolute
/// `gitdir:` pointer inside it resolves in the builder container.
///
/// Returns an empty vec for a plain repo — that is the shell's behaviour
/// (`GIT_DOCKER_MOUNTS=()`), not a failure. Deliberately does NOT use
/// [`crate::worktree`], which computes a strictly larger mount set; see the
/// module docs.
pub fn shell_git_mounts(repo_root: &Path) -> Vec<PathBuf> {
    let dot_git = repo_root.join(".git");
    if !dot_git.is_file() {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let raw = text
        .trim()
        .strip_prefix("gitdir: ")
        .unwrap_or_else(|| text.trim())
        .trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let gitdir = if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        repo_root.join(raw)
    };
    let gitdir = std::fs::canonicalize(&gitdir).unwrap_or(gitdir);

    // commondir holds a relative path like "../.."; fall back to gitdir/../..
    // exactly as the shell does.
    let main_git = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = c.trim();
            let joined = gitdir.join(c);
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
        Err(_) => {
            let joined = gitdir.join("../..");
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
    };

    vec![gitdir, main_git]
}

// ───────────────────────────────────────────────────────────────────────────
// Plan
// ───────────────────────────────────────────────────────────────────────────

/// A native `nix build`, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePlan {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    /// `dest` is `rm -rf`'d before the build: `nix build --out-link` refuses to
    /// clobber a non-symlink, and past Docker builds left root-owned files here.
    pub clear_dest: PathBuf,
}

/// A `docker run` of the `nixos/nix` builder, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerPlan {
    pub argv: Vec<String>,
    pub builder_tag: String,
    pub docker_platform: String,
    pub nix_volume: String,
    pub workdir: String,
    pub container_ref: String,
    pub inner_script: String,
    pub worktree_detected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Native(NativePlan),
    Docker(DockerPlan),
}

impl Plan {
    /// The full argv, for `--dry-run` printing and for diffing against the
    /// shell. Shell-quoted well enough to paste, not to be re-parsed.
    pub fn render(&self) -> String {
        let quote = |s: &String| {
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:=#@,+".contains(c))
            {
                s.clone()
            } else {
                format!("'{}'", s.replace('\'', r"'\''"))
            }
        };
        match self {
            Plan::Native(p) => format!(
                "(cd {} && {})",
                p.cwd.display(),
                p.argv.iter().map(quote).collect::<Vec<_>>().join(" ")
            ),
            Plan::Docker(p) => p.argv.iter().map(quote).collect::<Vec<_>>().join(" "),
        }
    }
}

/// The builder image. `nixos/nix:latest`, hard-coded in the shell; taken from
/// the CI profile's `builder.base_image` when one is supplied so the value is
/// data rather than a constant on this path too.
pub const DEFAULT_BUILDER_TAG: &str = "nixos/nix:latest";

/// Inputs a [`Plan`] needs that come from outside the [`BuildSpec`].
#[derive(Debug, Clone)]
pub struct PlanEnv {
    pub strategy: BuildStrategy,
    pub resources: DockerResources,
    /// Profile `build_strategy.docker_cache_volume` template. Empty ⇒ fall
    /// back to [`platform::nix_store_volume`], i.e. the shell's hard-coded
    /// `firestream-nix-store-<arch>`.
    pub docker_cache_volume_template: String,
    /// Profile `builder.base_image`. Empty ⇒ [`DEFAULT_BUILDER_TAG`].
    pub builder_tag: String,
}

impl Default for PlanEnv {
    fn default() -> Self {
        Self {
            strategy: BuildStrategy::Native,
            resources: DockerResources::default(),
            docker_cache_volume_template: String::new(),
            builder_tag: String::new(),
        }
    }
}

/// Pure. No filesystem writes, no process spawns — the one exception is
/// reading `.git` and `commondir` for the worktree mounts (which the shell
/// also does at this point) and canonicalising `flake_dir` / `dirname(dest)`.
pub fn plan(spec: &BuildSpec, env: &PlanEnv) -> Result<Plan, BuildError> {
    match env.strategy {
        BuildStrategy::Native => Ok(Plan::Native(plan_native(spec))),
        BuildStrategy::Docker => Ok(Plan::Docker(plan_docker(spec, env)?)),
    }
}

fn plan_native(spec: &BuildSpec) -> NativePlan {
    // `--dir` / `--sock` are accepted for signature parity and change nothing
    // on the native path — same as fs_nix_build_native.
    NativePlan {
        cwd: spec.flake_dir.clone(),
        clear_dest: spec.dest.clone(),
        argv: vec![
            "nix".into(),
            "build".into(),
            spec.flake_ref.clone(),
            "--out-link".into(),
            spec.dest.display().to_string(),
            "-L".into(),
            "--no-update-lock-file".into(),
            "--extra-experimental-features".into(),
            "nix-command flakes".into(),
        ],
    }
}

fn plan_docker(spec: &BuildSpec, env: &PlanEnv) -> Result<DockerPlan, BuildError> {
    let docker_platform = platform::docker_platform(&spec.target_arch);
    let nix_volume = if env.docker_cache_volume_template.is_empty() {
        platform::nix_store_volume(&spec.target_arch)
    } else {
        platform::docker_cache_volume(&env.docker_cache_volume_template, &spec.target_arch)
    };
    let builder_tag = if env.builder_tag.is_empty() {
        DEFAULT_BUILDER_TAG.to_string()
    } else {
        env.builder_tag.clone()
    };

    let dest_parent = spec
        .dest
        .parent()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .ok_or_else(|| BuildError::Other(format!("dest has no parent: {}", spec.dest.display())))?;
    let dest_base = spec
        .dest
        .file_name()
        .ok_or_else(|| BuildError::Other(format!("dest has no basename: {}", spec.dest.display())))?
        .to_string_lossy()
        .to_string();

    let mut argv: Vec<String> = vec![
        "docker".into(),
        "run".into(),
        "--rm".into(),
        "--platform".into(),
        docker_platform.clone(),
        "-v".into(),
        format!("{}:/out", dest_parent.display()),
        "--mount".into(),
        format!("type=volume,source={nix_volume},target=/nix"),
    ];

    if let Some(c) = &env.resources.cpus {
        argv.push("--cpus".into());
        argv.push(c.clone());
    }
    if let Some(m) = &env.resources.memory {
        argv.push("--memory".into());
        argv.push(m.clone());
    }
    if let Some(s) = &env.resources.swap {
        argv.push("--memory-swap".into());
        argv.push(s.clone());
    }

    if spec.docker_sock {
        argv.push("-v".into());
        argv.push("/var/run/docker.sock:/var/run/docker.sock".into());
    }

    // A /nix/store snapshot (external consumer, no checkout) goes to /flake; a
    // live working tree keeps its original path so worktree .git pointers
    // resolve.
    let flake_dir_str = spec.flake_dir.display().to_string();
    let (workdir, container_ref, worktree_detected);
    if flake_dir_str.starts_with("/nix/store/") {
        argv.push("-v".into());
        argv.push(format!("{flake_dir_str}:/flake:ro"));
        workdir = "/flake".to_string();
        container_ref = match spec.flake_ref.strip_prefix(".#") {
            Some(rest) => format!("/flake#{rest}"),
            None => spec.flake_ref.clone(),
        };
        worktree_detected = false;
    } else {
        let phys = std::fs::canonicalize(&spec.flake_dir).unwrap_or_else(|_| spec.flake_dir.clone());
        let phys_str = phys.display().to_string();
        argv.push("-v".into());
        argv.push(format!("{phys_str}:{phys_str}:ro"));
        workdir = phys_str;
        container_ref = spec.flake_ref.clone();
        let mounts = shell_git_mounts(&phys);
        worktree_detected = !mounts.is_empty();
        for m in mounts {
            argv.push("-v".into());
            argv.push(format!("{0}:{0}:ro", m.display()));
        }
    }

    let inner_script = inner_script(&container_ref, &dest_base, spec.output);

    argv.push("-w".into());
    argv.push(workdir.clone());
    argv.push(builder_tag.clone());
    argv.push("sh".into());
    argv.push("-c".into());
    argv.push(inner_script.clone());

    Ok(DockerPlan {
        argv,
        builder_tag,
        docker_platform,
        nix_volume,
        workdir,
        container_ref,
        inner_script,
        worktree_detected,
    })
}

/// The `sh -c` body, character-for-character equivalent to the two heredocs in
/// `fs_nix_build_docker`. The directory branch copies to a container-local
/// temp first because Nix store output is read-only and `chmod` fails on macOS
/// Docker volume mounts.
fn inner_script(container_ref: &str, dest_base: &str, output: Output) -> String {
    let head = "set -eu\n\n\
        echo \"experimental-features = nix-command flakes\" >> /etc/nix/nix.conf\n\
        git config --global --add safe.directory \"*\" 2>/dev/null || true\n\n";
    match output {
        Output::Dir => format!(
            "{head}echo \">>> Building {container_ref}...\"\n\
             nix build \"{container_ref}\" -o /tmp/result -L --no-update-lock-file\n\n\
             rm -rf \"/out/{dest_base}\"\n\
             mkdir -p \"/out/{dest_base}\"\n\
             cp -rL /tmp/result /tmp/result-writable\n\
             chmod -R u+w /tmp/result-writable\n\
             cp -r /tmp/result-writable/* \"/out/{dest_base}/\"\n\
             rm -rf /tmp/result-writable\n\n\
             echo \">>> Build successful\"\n"
        ),
        Output::Tarball => format!(
            "{head}echo \">>> Building {container_ref}...\"\n\
             nix build \"{container_ref}\" -o /tmp/result -L --no-update-lock-file\n\n\
             cp -L /tmp/result \"/out/{dest_base}\"\n\n\
             if [ ! -s \"/out/{dest_base}\" ]; then\n\
             \x20   echo \"ERROR: Output file empty or missing\" >&2\n\
             \x20   exit 1\n\
             fi\n\n\
             echo \">>> Build successful\"\n"
        ),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Batch lock — mirror of container-images.sh's $BUILD_OUTPUT_DIR/.build-batch.lock
// ───────────────────────────────────────────────────────────────────────────

/// `mkdir`-based mutual exclusion with stale-PID recovery, released on drop.
///
/// Uses the SAME path as the bash (`<build_output_dir>/.build-batch.lock`) so
/// the two implementations exclude each other. That is a property worth
/// having during a strangler: an opted-in Rust build and a default bash build
/// cannot race over `_build/`.
#[derive(Debug)]
pub struct BatchLock {
    path: PathBuf,
}

impl BatchLock {
    pub const NAME: &'static str = ".build-batch.lock";

    pub fn acquire(build_output_dir: &Path) -> Result<Self, BuildError> {
        let path = build_output_dir.join(Self::NAME);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = std::fs::read_to_string(path.join("pid"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !pid.is_empty() && pid_alive(&pid) {
                    return Err(BuildError::LockHeld { pid, lock: path });
                }
                eprintln!(
                    "  ! Removing stale lock (PID {})",
                    if pid.is_empty() { "unknown" } else { &pid }
                );
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir(&path).map_err(io_err(path.clone()))?;
            }
            Err(e) => return Err(io_err(path.clone())(e)),
        }
        std::fs::write(path.join("pid"), std::process::id().to_string())
            .map_err(io_err(path.join("pid")))?;
        Ok(Self { path })
    }
}

impl Drop for BatchLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `kill -0 <pid>`, the shell's liveness probe. Shelled out rather than
/// `libc::kill` so this compiles unchanged on every platform the toolkit
/// targets and matches the bash semantics exactly (including "not mine ⇒
/// EPERM ⇒ alive").
fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ───────────────────────────────────────────────────────────────────────────
// docker load
// ───────────────────────────────────────────────────────────────────────────

/// `docker load < <tarball>`, returning the loaded tag.
///
/// Tag extraction mirrors the shell's
/// `sed -n 's/.*Loaded image: //p' | awk '{print $1}' | tail -1` — LAST match
/// wins, first whitespace-delimited field only, `(unknown)` when nothing
/// matched but the load succeeded.
pub async fn docker_load(tarball: &Path) -> Result<String, BuildError> {
    let file = std::fs::File::open(tarball).map_err(io_err(tarball))?;
    let out = tokio::process::Command::new("docker")
        .arg("load")
        .stdin(std::process::Stdio::from(file))
        .output()
        .await
        .map_err(io_err(tarball))?;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(BuildError::Other(format!(
            "docker load failed ({}): {}",
            out.status.code().unwrap_or(-1),
            combined.trim()
        )));
    }
    Ok(parse_loaded_tag(&combined))
}

/// Split out so the sed/awk/tail pipeline is unit-testable without Docker.
pub fn parse_loaded_tag(load_output: &str) -> String {
    load_output
        .lines()
        .filter_map(|l| l.split("Loaded image: ").nth(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .next_back()
        .unwrap_or("(unknown)")
        .to_string()
}

// ───────────────────────────────────────────────────────────────────────────
// Execution
// ───────────────────────────────────────────────────────────────────────────

/// Run a [`Plan`], teeing combined output to the terminal and to `log`.
pub async fn execute(plan: &Plan, log: Option<&Path>) -> Result<i32, BuildError> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut cmd = match plan {
        Plan::Native(p) => {
            warn_if_untrusted_nix_user();
            // `nix build --out-link` refuses to clobber a non-symlink, and
            // past Docker builds left root-owned regular files here.
            if p.clear_dest.exists() || p.clear_dest.symlink_metadata().is_ok() {
                remove_any(&p.clear_dest).map_err(|e| {
                    BuildError::Other(format!(
                        "Cannot remove stale build artifact: {}: {e}. It may be root-owned \
                         from a previous Docker build. Try: sudo rm -rf {}",
                        p.clear_dest.display(),
                        p.clear_dest.display()
                    ))
                })?;
            }
            let mut c = tokio::process::Command::new(&p.argv[0]);
            c.args(&p.argv[1..]).current_dir(&p.cwd);
            c
        }
        Plan::Docker(p) => {
            let mut c = tokio::process::Command::new(&p.argv[0]);
            c.args(&p.argv[1..]);
            c
        }
    };

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| BuildError::Other(format!("spawn failed: {e}")))?;

    let mut sink = match log {
        Some(p) => Some(
            tokio::fs::File::create(p)
                .await
                .map_err(io_err(p.to_path_buf()))?,
        ),
        None => None,
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut lines: Vec<Box<dyn tokio::io::AsyncBufRead + Unpin + Send>> = Vec::new();
    if let Some(s) = stdout {
        lines.push(Box::new(BufReader::new(s)));
    }
    if let Some(s) = stderr {
        lines.push(Box::new(BufReader::new(s)));
    }

    // Sequential drain is adequate: `nix build -L` writes to stderr and the
    // docker builder's `sh -c` writes to stdout, and both are line-oriented.
    for mut r in lines {
        let mut buf = String::new();
        loop {
            buf.clear();
            match r.read_line(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    eprint!("{buf}");
                    if let Some(f) = sink.as_mut() {
                        use tokio::io::AsyncWriteExt;
                        let _ = f.write_all(buf.as_bytes()).await;
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| BuildError::Other(format!("wait failed: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

/// Port of `fs_nix_build_native`'s trusted-users probe.
///
/// An untrusted user silently loses flake-supplied `extra-substituters` and
/// ends up rebuilding the world from source — which on this repo is the
/// difference between minutes and hours, with no error to explain it. Warn,
/// never fail; the probe itself is best-effort.
fn warn_if_untrusted_nix_user() {
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() || user == "root" {
        return;
    }
    let trusted = std::process::Command::new("nix")
        .args(["config", "show", "trusted-users"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if trusted.is_empty() {
        return;
    }
    let fields: Vec<&str> = trusted.split_whitespace().collect();
    if fields.contains(&"*") || fields.contains(&user.as_str()) {
        return;
    }
    eprintln!(
        "  ! '{user}' is not in nix trusted-users; flake-supplied substituters will be \
         ignored (expect source rebuilds)"
    );
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Batch summary
// ───────────────────────────────────────────────────────────────────────────

/// Per-package outcome, in batch order.
#[derive(Debug, Clone)]
pub struct PackageResult {
    pub package: String,
    pub container: String,
    pub succeeded: bool,
    /// The tag `docker load` reported, when the image was loaded.
    pub image_tag: Option<String>,
    /// Populated on failure, or when `--no-load` skipped the load step.
    pub note: Option<String>,
}

/// The batch summary box. Byte-compatible layout with `container-images.sh`'s
/// (48-column body, `Succeeded` / `Failed` / `Duration` rows) — colour is
/// dropped, because a summary that only *looks* the same is worse than one
/// that plainly is not.
pub fn render_summary(results: &[PackageResult], duration_secs: u64) -> String {
    let ok = results.iter().filter(|r| r.succeeded).count();
    let failed = results.len() - ok;
    let mut s = String::new();
    s.push_str("  ╔══════════════════════════════════════════════════╗\n");
    s.push_str(&format!("  ║  {:<48}║\n", "BUILD SUMMARY"));
    s.push_str("  ╠══════════════════════════════════════════════════╣\n");
    s.push_str(&format!("  ║  Succeeded   {:<38}║\n", ok));
    if failed > 0 {
        s.push_str(&format!("  ║  Failed      {:<38}║\n", failed));
    }
    s.push_str(&format!(
        "  ║  Duration    {:<38}║\n",
        format!("{duration_secs}s")
    ));
    s.push_str("  ╚══════════════════════════════════════════════════╝\n");
    s
}

/// Wall-clock helper so callers don't each reimplement the `date +%s` deltas.
pub struct Stopwatch(Instant);

impl Stopwatch {
    pub fn start() -> Self {
        Self(Instant::now())
    }
    pub fn secs(&self) -> u64 {
        self.0.elapsed().as_secs()
    }
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self::start()
    }
}

/// `_build/` layout, mirroring the two scripts:
/// * images:   `<build_output_dir>/<pkg>/<pkg>.tar.gz` + `build.log`
/// * manifest: `<build_output_dir>/manifest/`
/// * sbom:     `<build_output_dir>/sbom-<container>/`
pub fn image_dest(build_output_dir: &Path, pkg: &str) -> PathBuf {
    build_output_dir.join(pkg).join(format!("{pkg}.tar.gz"))
}

pub fn manifest_dest(build_output_dir: &Path, container: Option<&str>) -> (PathBuf, String) {
    match container {
        None => (build_output_dir.join("manifest"), ".#manifest".to_string()),
        Some(c) => (
            build_output_dir.join(format!("sbom-{c}")),
            format!(".#sbom.{c}"),
        ),
    }
}

/// Env-var opt-in shared with the shell scripts. `bash` (default) | `rust`.
pub const ENV_BUILD_IMPL: &str = "FIRESTREAM_BUILD_IMPL";

/// Extra key/values a caller may want to print alongside a dry run.
pub fn plan_facts(plan: &Plan) -> BTreeMap<&'static str, String> {
    let mut m = BTreeMap::new();
    match plan {
        Plan::Native(p) => {
            m.insert("strategy", "native".to_string());
            m.insert("cwd", p.cwd.display().to_string());
            m.insert("out_link", p.clear_dest.display().to_string());
        }
        Plan::Docker(p) => {
            m.insert("strategy", "docker".to_string());
            m.insert("builder", p.builder_tag.clone());
            m.insert("platform", p.docker_platform.clone());
            m.insert("nix_volume", p.nix_volume.clone());
            m.insert("workdir", p.workdir.clone());
            m.insert("flake_ref", p.container_ref.clone());
            m.insert("worktree", p.worktree_detected.to_string());
        }
    }
    m
}

#[cfg(test)]
mod tests;
