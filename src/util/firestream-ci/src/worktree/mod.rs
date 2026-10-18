//! Pattern #10 — worktree → libgit2-valid mount chain with
//! **symlink-divergence walk**. Mirrors `build/_common.sh::resolve_git_mounts`
//! (lines 36-112).
//!
//! The libgit2 string-based path normalization fails when a worktree's
//! `gitdir` pointer (or a submodule config's `worktree =` value) traverses
//! a symlink: libgit2 concatenates the relative path string-wise and
//! expects the literal result to exist on disk. Docker, in turn, mounts
//! real paths only. The fix is to compute BOTH the logical (caller-passed)
//! and physical (canonicalized) paths, walk both from the tail, and emit
//! bind-mounts at any ancestor where they diverge.
//!
//! Outputs a typed `Mount` list. Rendering to `docker -v` flags is left to
//! the caller because the same mount data is reused for container API
//! calls in `oci::source_sync` (Phase 5).

mod divergence;
mod resolve;

pub use divergence::{DivergencePoint, divergence_walk};
pub use resolve::{Mount, MountKind};

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("worktree: git error on `{path}`: {source}")]
    Git {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },

    #[error("worktree: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("worktree: gitdir `{0}` does not exist")]
    GitdirMissing(PathBuf),

    #[error("worktree: workdir for repository at `{0}` is bare")]
    BareRepo(PathBuf),
}

/// Typed handle for a git worktree. Cheap to open — under the hood it's
/// a single `git2::Repository::open`. The returned mounts are deterministic
/// for a given on-disk state, so callers can cache them.
pub struct Worktree {
    /// The path the caller passed in (logical view — may include symlinks).
    logical_path: PathBuf,
    /// The same path canonicalized (physical view).
    physical_path: PathBuf,
    repo: git2::Repository,
}

impl Worktree {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let logical_path = path.as_ref().to_path_buf();
        // Canonicalize separately — the bash never assumed the caller
        // pre-canonicalized, and the symlink-divergence walk needs both.
        let physical_path = std::fs::canonicalize(&logical_path).map_err(|source| Error::Io {
            path: logical_path.clone(),
            source,
        })?;
        let repo = git2::Repository::open(&physical_path).map_err(|source| Error::Git {
            path: physical_path.clone(),
            source,
        })?;
        Ok(Self {
            logical_path,
            physical_path,
            repo,
        })
    }

    pub fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub fn physical_path(&self) -> &Path {
        &self.physical_path
    }

    pub fn is_bare(&self) -> bool {
        self.repo.is_bare()
    }

    pub fn is_worktree(&self) -> bool {
        // git2 exposes this via `state()` indirectly — the cleanest signal
        // is whether the `.git` path is a file (worktree pointer) vs
        // directory (plain repo).
        self.physical_path.join(".git").is_file()
    }

    /// HEAD's full ref name, or None on detached HEAD.
    pub fn head_branch(&self) -> Option<String> {
        let head = self.repo.head().ok()?;
        if !head.is_branch() {
            return None;
        }
        head.shorthand().map(|s| s.to_string())
    }

    /// Compute the minimum set of bind-mounts libgit2 needs to operate on
    /// this worktree from inside a container. Includes:
    ///   * The workdir + .git directory (plain repo case)
    ///   * Worktree gitdir + commondir (worktree case)
    ///   * Submodule `modules/<name>/` directories
    ///   * Both logical AND physical mounts where symlink divergence
    ///     surfaces (the libgit2 bug surface)
    pub fn container_mounts(&self) -> Result<Vec<Mount>, Error> {
        resolve::container_mounts(self)
    }
}
