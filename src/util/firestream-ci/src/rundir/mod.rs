//! Pattern #15 — per-run output dir. Successor to the bash
//! `allocate_run_dir`/`ensure_run_dir`/`_prune_old_runs` trio that used to
//! live in `bin/build/_common.sh`; those are gone, this module is the only
//! authority now. What survives on the shell side is the repo-root walk
//! (`_common.sh:28-47`) and `BUILD_OUTPUT_DIR` (`_common.sh:54`), mirrored
//! here by [`find_repo_root`] / [`default_build_root`].
//!
//! Layout: `<base>/<YYYY-MM-DD>/<sha>/<epoch>/` with `spans/`, `logs/`,
//! `profiles/`, `artifacts/` subdirs. The `.sentinel` file is the
//! host↔container sync handshake: the host writes it on `create`; the
//! container `verify`s it points at the same physical dir. Without that
//! handshake, a misconfigured `docker -v` could quietly route container
//! writes to a different host directory than the host expects.
//!
//! Pruning is mtime-desc + recent-sentinel skip. A run dir whose sentinel
//! was touched in the last 60s is assumed live and never pruned, so a
//! concurrent CI invocation can't reap its own siblings mid-run.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use thiserror::Error;
use tokio::fs;

const SENTINEL_NAME: &str = ".sentinel";
const SUBDIRS: &[&str] = &["spans", "logs", "profiles", "artifacts"];

/// Skip pruning a run dir whose sentinel was modified less than this many
/// seconds ago. 60s comfortably exceeds the time a fresh CI invocation
/// takes to allocate + start emitting work, so a near-concurrent prune
/// can't reap an active run.
const ACTIVE_SENTINEL_GRACE_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum Error {
    #[error("rundir: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("rundir: sentinel mismatch — host wrote `{host}` but container sees `{container}`")]
    SentinelMismatch { host: String, container: String },

    #[error("rundir: sentinel missing at `{path}`")]
    SentinelMissing { path: PathBuf },
}

/// A per-run output directory + its conventional subdirs. All paths are
/// absolute; lifetime is the duration of the run.
#[derive(Debug, Clone)]
pub struct RunDir {
    path: PathBuf,
    spans: PathBuf,
    logs: PathBuf,
    profiles: PathBuf,
    artifacts: PathBuf,
}

impl RunDir {
    /// Allocate a new run dir under `base`. Path layout matches the bash:
    /// `<base>/<YYYY-MM-DD>/<sha_hint>/<epoch_seconds>/`. The `sha_hint`
    /// is the caller's short SHA or "nogit" when the caller doesn't know.
    /// Creates all subdirs and writes the sentinel.
    pub async fn create(base: impl AsRef<Path>, sha_hint: &str) -> Result<Self, Error> {
        let base = base.as_ref();
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = base.join(&date).join(sha_hint).join(epoch.to_string());

        fs::create_dir_all(&path)
            .await
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        for sub in SUBDIRS {
            let p = path.join(sub);
            fs::create_dir_all(&p)
                .await
                .map_err(|source| Error::Io { path: p, source })?;
        }
        // Sentinel contents = the physical run-dir path. The container side
        // recomputes its physical view of the same path and compares.
        let sentinel = path.join(SENTINEL_NAME);
        fs::write(&sentinel, path.to_string_lossy().as_bytes())
            .await
            .map_err(|source| Error::Io {
                path: sentinel,
                source,
            })?;

        Ok(Self::from_path(path))
    }

    /// Adopt an existing run dir (inherit from environment in the
    /// container). Recreates the subdirs idempotently — they exist after
    /// `create()`, but a fresh container may have rm -rf'd them between
    /// host stages.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        for sub in SUBDIRS {
            let p = path.join(sub);
            fs::create_dir_all(&p)
                .await
                .map_err(|source| Error::Io { path: p, source })?;
        }
        Ok(Self::from_path(path))
    }

    /// Verify the host-written sentinel resolves to the same path the
    /// container is looking at. Use from container-side code immediately
    /// after `open()` to catch a mis-mounted `-v` volume.
    pub async fn verify_sentinel(&self) -> Result<(), Error> {
        let sentinel = self.path.join(SENTINEL_NAME);
        let host = fs::read_to_string(&sentinel).await.map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::SentinelMissing {
                    path: sentinel.clone(),
                }
            } else {
                Error::Io {
                    path: sentinel.clone(),
                    source,
                }
            }
        })?;
        let host = host.trim().to_string();
        let container = self.path.to_string_lossy().to_string();
        if host != container {
            return Err(Error::SentinelMismatch { host, container });
        }
        Ok(())
    }

    /// Refresh the sentinel mtime so the pruner counts this run as active.
    /// Cheap; safe to call on a heartbeat.
    pub async fn touch_sentinel(&self) -> Result<(), Error> {
        let sentinel = self.path.join(SENTINEL_NAME);
        // No `utimens` in tokio::fs; write the same bytes back which bumps
        // mtime on every Linux filesystem.
        let content = self.path.to_string_lossy().to_string();
        fs::write(&sentinel, content.as_bytes())
            .await
            .map_err(|source| Error::Io {
                path: sentinel,
                source,
            })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn spans_dir(&self) -> &Path {
        &self.spans
    }

    pub fn logs_dir(&self) -> &Path {
        &self.logs
    }

    pub fn profiles_dir(&self) -> &Path {
        &self.profiles
    }

    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts
    }

    fn from_path(path: PathBuf) -> Self {
        let spans = path.join("spans");
        let logs = path.join("logs");
        let profiles = path.join("profiles");
        let artifacts = path.join("artifacts");
        Self {
            path,
            spans,
            logs,
            profiles,
            artifacts,
        }
    }

    /// Number of run dirs under `base` — exactly the set `prune` walks
    /// (`<date>/<sha>/<epoch>` shape, date/epoch name-validated). Lets
    /// `firestream-ci sweep --dry-run` report what a prune would touch without
    /// duplicating the walk.
    pub async fn count(base: impl AsRef<Path>) -> Result<usize, Error> {
        Ok(collect_run_dirs(base.as_ref()).await?.len())
    }

    /// Delete the oldest run dirs under `base` so at most `keep` remain.
    /// "Oldest" = oldest sentinel mtime (falls back to created time if no
    /// sentinel). Run dirs whose sentinel was touched in the last
    /// `ACTIVE_SENTINEL_GRACE_SECS` are never pruned — protects concurrent
    /// runs from reaping each other. Returns the number of dirs removed.
    pub async fn prune(base: impl AsRef<Path>, keep: usize) -> Result<usize, Error> {
        let base = base.as_ref().to_path_buf();
        let runs = collect_run_dirs(&base).await?;
        if runs.len() <= keep {
            return Ok(0);
        }

        // Sort newest-first by sentinel mtime, then drop the suffix.
        let mut annotated: Vec<(PathBuf, SystemTime)> = Vec::with_capacity(runs.len());
        for r in runs {
            let mtime = sentinel_mtime(&r).await.unwrap_or(SystemTime::UNIX_EPOCH);
            annotated.push((r, mtime));
        }
        annotated.sort_by(|a, b| b.1.cmp(&a.1));

        let now = SystemTime::now();
        let grace = Duration::from_secs(ACTIVE_SENTINEL_GRACE_SECS);
        let mut removed = 0usize;
        for (path, mtime) in annotated.into_iter().skip(keep) {
            if let Ok(age) = now.duration_since(mtime) {
                if age < grace {
                    // Treat as live — skip but keep iterating; a much
                    // older entry further down may still be reapable.
                    continue;
                }
            }
            if remove_run_dir(&path).await.is_ok() {
                removed += 1;
            }
        }

        // Best-effort: prune empty <date>/<sha> dirs left behind. Failures
        // here are not fatal — they'll be cleaned on the next prune.
        let _ = prune_empty_intermediate_dirs(&base).await;
        Ok(removed)
    }
}

/// Walk up from `start` looking for the repo-root markers `flake.nix` +
/// `src/containers/firestream`, then fall back to `/workspace` (the
/// in-container bind mount). Mirror of `bin/build/_common.sh:28-47`.
///
/// Returns `None` when neither is found; callers decide the fallback.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    loop {
        if dir.join("flake.nix").is_file() && dir.join("src/containers/firestream").is_dir() {
            return Some(dir);
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => break,
        }
    }
    if Path::new("/workspace/src/containers/firestream").is_dir() {
        return Some(PathBuf::from("/workspace"));
    }
    None
}

/// The canonical `_build/` base: **repo-root-anchored, never cwd-relative**.
///
/// This exists because `cd src/util && cargo run -p firestream-ci ...` (what
/// `make build-util` / `make test-util` do) used to mint a second, stray
/// `src/util/_build/` tree. There is exactly one `_build/`, at the repo root.
///
/// Deliberately does **not** consult `BUILD_OUTPUT_DIR`. That variable is
/// overloaded in this codebase: `bin/firestream-ci.rs` treats it as an
/// *already-allocated rundir* to hand to [`RunDir::open`] (the container
/// adopt path), while `build_output_dir_for` treats it as the `_build` base.
/// Reading it here would collapse the two meanings — don't.
pub fn default_build_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    find_repo_root(&cwd).unwrap_or(cwd).join("_build")
}

async fn sentinel_mtime(run: &Path) -> Result<SystemTime, std::io::Error> {
    let meta = fs::metadata(run.join(SENTINEL_NAME)).await?;
    meta.modified()
}

/// Enumerate every `<base>/<date>/<sha>/<epoch>/` triple under `base`.
/// Anything that doesn't fit the YYYY-MM-DD/<sha>/<epoch> shape is ignored
/// — the prune scope is exactly what `allocate_run_dir` creates.
async fn collect_run_dirs(base: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    if !base.exists() {
        return Ok(out);
    }
    let mut dates = fs::read_dir(base).await.map_err(|source| Error::Io {
        path: base.to_path_buf(),
        source,
    })?;
    while let Some(date_entry) = dates.next_entry().await.map_err(|source| Error::Io {
        path: base.to_path_buf(),
        source,
    })? {
        let date_path = date_entry.path();
        if !is_date_dir(&date_path) {
            continue;
        }
        let mut shas = match fs::read_dir(&date_path).await {
            Ok(x) => x,
            Err(_) => continue,
        };
        while let Some(sha_entry) = shas.next_entry().await.unwrap_or(None) {
            let sha_path = sha_entry.path();
            if !sha_path.is_dir() {
                continue;
            }
            let mut epochs = match fs::read_dir(&sha_path).await {
                Ok(x) => x,
                Err(_) => continue,
            };
            while let Some(epoch_entry) = epochs.next_entry().await.unwrap_or(None) {
                let epoch_path = epoch_entry.path();
                if epoch_path.is_dir() && is_epoch_dir(&epoch_path) {
                    out.push(epoch_path);
                }
            }
        }
    }
    Ok(out)
}

fn is_date_dir(p: &Path) -> bool {
    p.is_dir()
        && p.file_name()
            .and_then(|n| n.to_str())
            .map(|s| {
                // YYYY-MM-DD shape: 10 chars, dashes at indices 4 and 7.
                s.len() == 10
                    && s.as_bytes().get(4) == Some(&b'-')
                    && s.as_bytes().get(7) == Some(&b'-')
                    && s.chars().enumerate().all(|(i, c)| match i {
                        4 | 7 => c == '-',
                        _ => c.is_ascii_digit(),
                    })
            })
            .unwrap_or(false)
}

fn is_epoch_dir(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

async fn remove_run_dir(p: &Path) -> std::io::Result<()> {
    // Mirror the bash: chmod -R u+w first because old root-owned files
    // from a previous CI container may not be removable otherwise. tokio
    // doesn't have a recursive chmod; shell out via std::fs and accept the
    // sync hit — pruning runs rarely, file count per dir is small.
    let p = p.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _ = ensure_writable_recursive(&p);
        std::fs::remove_dir_all(&p)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn ensure_writable_recursive(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in walkdir::WalkDir::new(p) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o200);
            let _ = std::fs::set_permissions(entry.path(), perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = p;
    }
    Ok(())
}

async fn prune_empty_intermediate_dirs(base: &Path) -> std::io::Result<()> {
    // Two passes: <date>/<sha>, then <date>. Empty-only removal.
    let mut dates = match fs::read_dir(base).await {
        Ok(x) => x,
        Err(_) => return Ok(()),
    };
    while let Some(d) = dates.next_entry().await? {
        let dp = d.path();
        if !is_date_dir(&dp) {
            continue;
        }
        let mut shas = match fs::read_dir(&dp).await {
            Ok(x) => x,
            Err(_) => continue,
        };
        while let Some(s) = shas.next_entry().await? {
            let sp = s.path();
            if sp.is_dir() && is_dir_empty(&sp).await {
                let _ = fs::remove_dir(&sp).await;
            }
        }
        if is_dir_empty(&dp).await {
            let _ = fs::remove_dir(&dp).await;
        }
    }
    Ok(())
}

async fn is_dir_empty(p: &Path) -> bool {
    match fs::read_dir(p).await {
        Ok(mut r) => r.next_entry().await.ok().flatten().is_none(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn create_makes_layout_and_sentinel() {
        let base = tempdir().unwrap();
        let r = RunDir::create(base.path(), "abc12345").await.unwrap();
        assert!(r.path().exists());
        assert!(r.spans_dir().is_dir());
        assert!(r.logs_dir().is_dir());
        assert!(r.profiles_dir().is_dir());
        assert!(r.artifacts_dir().is_dir());
        assert!(r.path().join(SENTINEL_NAME).exists());

        let sentinel_text = std::fs::read_to_string(r.path().join(SENTINEL_NAME)).unwrap();
        assert_eq!(sentinel_text, r.path().to_string_lossy());

        // Path shape: <base>/<date>/<sha>/<epoch>
        let rel = r.path().strip_prefix(base.path()).unwrap();
        let comps: Vec<_> = rel.components().collect();
        assert_eq!(comps.len(), 3);
    }

    #[tokio::test]
    async fn verify_sentinel_round_trip() {
        let base = tempdir().unwrap();
        let r = RunDir::create(base.path(), "abc12345").await.unwrap();
        r.verify_sentinel().await.unwrap();

        // Simulate divergent host/container view by rewriting the sentinel.
        std::fs::write(r.path().join(SENTINEL_NAME), "/not/the/right/path").unwrap();
        let err = r.verify_sentinel().await.unwrap_err();
        assert!(matches!(err, Error::SentinelMismatch { .. }));
    }

    #[tokio::test]
    async fn prune_keeps_n_most_recent() {
        let base = tempdir().unwrap();
        // Create three synthetic run dirs with deterministic epochs.
        for epoch in &[100u64, 200, 300] {
            let p = base
                .path()
                .join("2026-05-29")
                .join("aaaaaaaa")
                .join(epoch.to_string());
            std::fs::create_dir_all(&p).unwrap();
            let sentinel = p.join(SENTINEL_NAME);
            std::fs::write(&sentinel, p.to_string_lossy().as_bytes()).unwrap();
            // Set sentinel mtime well in the past so prune doesn't grace-skip.
            let secs_ago = 3600 + (300 - *epoch);
            set_mtime_past(&sentinel, secs_ago);
        }
        let removed = RunDir::prune(base.path(), 2).await.unwrap();
        assert_eq!(removed, 1);
        // The 100-epoch (oldest mtime) is gone; 200 + 300 remain.
        let date_dir = base.path().join("2026-05-29").join("aaaaaaaa");
        assert!(!date_dir.join("100").exists());
        assert!(date_dir.join("200").exists());
        assert!(date_dir.join("300").exists());
    }

    #[tokio::test]
    async fn prune_skips_recently_touched_sentinel() {
        let base = tempdir().unwrap();
        for epoch in &[100u64, 200] {
            let p = base
                .path()
                .join("2026-05-29")
                .join("bbbbbbbb")
                .join(epoch.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join(SENTINEL_NAME), b"x").unwrap();
            // Both sentinels are fresh — within grace.
        }
        let removed = RunDir::prune(base.path(), 1).await.unwrap();
        assert_eq!(removed, 0, "fresh sentinels must skip prune");
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let base = tempdir().unwrap();
        let r1 = RunDir::create(base.path(), "abc12345").await.unwrap();
        let r2 = RunDir::open(r1.path()).await.unwrap();
        assert_eq!(r1.path(), r2.path());
        assert!(r2.spans_dir().is_dir());
    }

    fn set_mtime_past(path: &Path, secs_ago: u64) {
        let when = SystemTime::now() - Duration::from_secs(secs_ago);
        let ft = filetime::FileTime::from_system_time(when);
        filetime::set_file_mtime(path, ft).expect("set_file_mtime");
    }
}
