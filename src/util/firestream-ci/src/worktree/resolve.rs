//! Build the container mount chain. Walks `git2::Repository` to discover
//! workdir, gitdir, commondir, and any submodule `modules/<name>/` dirs;
//! runs `divergence_walk` per path to surface the libgit2 bug surface.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{Error, Worktree, divergence::divergence_walk};

/// One bind-mount entry. The caller chooses the rendering (Docker `-v`,
/// bollard `HostConfig.binds`, etc.) — separating data from formatting
/// keeps the same `Mount` struct usable for both `docker create` and the
/// container-API path in `oci::source_sync` (Phase 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub kind: MountKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MountKind {
    /// Plain bind-mount: source path inside the container is the same as
    /// the host path. Default for git dirs.
    Bind,
    /// Symlink-divergence mount: source is the physical path, target is
    /// the logical path libgit2 expects to find. Without this entry,
    /// libgit2 fails to resolve a submodule or worktree pointer.
    SymlinkBind,
}

impl Mount {
    fn bind(p: PathBuf) -> Self {
        Self {
            source: p.clone(),
            target: p,
            kind: MountKind::Bind,
        }
    }

    fn symlink_bind(physical: PathBuf, logical: PathBuf) -> Self {
        Self {
            source: physical,
            target: logical,
            kind: MountKind::SymlinkBind,
        }
    }
}

pub(super) fn container_mounts(wt: &Worktree) -> Result<Vec<Mount>, Error> {
    let mut set: BTreeSet<(PathBuf, PathBuf, MountKind)> = BTreeSet::new();

    // 1. Repository workdir. Bare repos have no workdir; the bash never
    // sees this case (CI checks out a working tree), but we still surface
    // it as a typed error rather than panicking.
    let workdir = wt
        .repo
        .workdir()
        .ok_or_else(|| Error::BareRepo(wt.physical_path.clone()))?;
    add_mount(&mut set, Mount::bind(workdir.to_path_buf()));

    // 2. gitdir + commondir. For plain repos these are the same; for
    // worktrees commondir points back at the main repo's `.git`. git2
    // 0.19 doesn't expose commondir — read it from the `commondir` file
    // the way the bash does (lines 51-54), then canonicalize.
    let gitdir = wt.repo.path().to_path_buf();
    add_mount(&mut set, Mount::bind(gitdir.clone()));
    let commondir = read_commondir(&gitdir).unwrap_or_else(|| gitdir.clone());
    if commondir != gitdir {
        add_mount(&mut set, Mount::bind(commondir.clone()));
    }

    // 3. Symlink-divergence for the workdir itself. The bash check is at
    // line 89: `[[ "$abs_portion" != "$resolved" ]]` for the submodule
    // worktree value. We generalize to the top-level workdir too — a
    // developer can `cd` into the repo via a symlinked path, and `git2`'s
    // `Repository::open(physical)` works fine, but child processes
    // launched with the logical path embedded in env vars (like
    // `CARGO_MANIFEST_DIR`) carry the unresolved path through to libgit2
    // calls deeper in the build.
    let dp = divergence_walk(&wt.logical_path, &wt.physical_path);
    if dp.diverges {
        add_mount(
            &mut set,
            Mount::symlink_bind(dp.divergent_physical.clone(), dp.divergent_logical.clone()),
        );
    }

    // 4. Submodule gitdirs. For each submodule, the `modules/<name>/`
    // directory holds the actual git state + a `config` whose
    // `worktree = ...` value is the relative path back to the submodule's
    // working tree. The bash samples one submodule's config to detect
    // divergence (line 71-76); we walk every submodule for completeness
    // (cost: one config read per submodule, milliseconds).
    let modules_dir = commondir.join("modules");
    if modules_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&modules_dir)
            .max_depth(3)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_file() && entry.file_name() == "config" {
                let config_dir = match entry.path().parent() {
                    Some(d) => d.to_path_buf(),
                    None => continue,
                };
                // Mount this submodule gitdir explicitly. The outer
                // commondir mount covers it transitively, but a Docker
                // bind-mount of a subdir keeps the SymlinkBind divergence
                // check below independent of the parent layout.
                add_mount(&mut set, Mount::bind(config_dir.clone()));
                if let Some(worktree_value) = read_worktree_value(entry.path()) {
                    if let Some((logi_repo, phys_repo)) = resolve_submodule_divergence(
                        &config_dir,
                        &worktree_value,
                        &wt.physical_path,
                    ) {
                        add_mount(&mut set, Mount::symlink_bind(phys_repo, logi_repo));
                    }
                }
            }
        }
    }

    let mut out: Vec<Mount> = set
        .into_iter()
        .map(|(source, target, kind)| Mount {
            source,
            target,
            kind,
        })
        .collect();
    // Stable ordering by (source, target).
    out.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
    Ok(out)
}

fn add_mount(set: &mut BTreeSet<(PathBuf, PathBuf, MountKind)>, m: Mount) {
    set.insert((m.source, m.target, m.kind));
}

/// Read `.git/commondir`, resolve relative to `gitdir`, and canonicalize.
/// Returns `None` if the file is missing (plain repo) or unreadable.
fn read_commondir(gitdir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw = if trimmed.starts_with('/') {
        PathBuf::from(trimmed)
    } else {
        gitdir.join(trimmed)
    };
    std::fs::canonicalize(raw).ok()
}

/// Parse the `worktree = ...` line from a submodule's `config` file.
fn read_worktree_value(config_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_path).ok()?;
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("worktree =") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("worktree=") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// The bash equivalent (lines 78-110): given the submodule config dir and
/// the relative `worktree =` value, reconstruct the absolute logical path
/// and compare to its canonical form. If they diverge, return the
/// (logical_repo_root, physical_repo_root) pair to mount.
fn resolve_submodule_divergence(
    config_dir: &Path,
    worktree_relative: &str,
    repo_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    // The bash extracts the absolute-looking portion via:
    //   abs_portion="${sample_worktree##*/../}"
    //   [[ "$abs_portion" != /* ]] && abs_portion="/$abs_portion"
    // Mirror exactly — this is the format git itself writes.
    let abs_portion = match worktree_relative.rsplit_once("/../") {
        Some((_, tail)) => tail.to_string(),
        None => worktree_relative.to_string(),
    };
    let abs_portion = if abs_portion.starts_with('/') {
        PathBuf::from(abs_portion)
    } else {
        PathBuf::from(format!("/{abs_portion}"))
    };

    // Resolve what the relative path actually points to from inside the
    // config dir. This is the bash `(cd "$(dirname "$config_file")" && cd "$sample_worktree" 2>/dev/null && pwd -P)`.
    let resolved = std::fs::canonicalize(config_dir.join(worktree_relative)).ok()?;

    if abs_portion == resolved {
        return None;
    }

    let dp = divergence_walk(&abs_portion, &resolved);
    if !dp.diverges {
        return None;
    }

    // Bash maps abs_portion (logical) → resolved (physical) at the repo
    // level so the mount is broad enough to cover all sibling files but
    // narrow enough to avoid mounting /Volumes (which Docker Desktop
    // refuses for the root). We follow the same shape.
    let phys_repo = repo_root.to_path_buf();
    // logi_repo = logical_path + (repo_root - physical_path)
    let rel = repo_root.strip_prefix(&dp.divergent_physical).ok()?;
    let logi_repo = dp.divergent_logical.join(rel);
    Some((logi_repo, phys_repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_value_parser_handles_both_formats() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config");
        std::fs::write(
            &cfg,
            "[core]\n  bare = false\n  worktree = ../../../foo/bar\n",
        )
        .unwrap();
        assert_eq!(read_worktree_value(&cfg), Some("../../../foo/bar".into()));

        std::fs::write(&cfg, "[core]\nworktree=/abs/path\n").unwrap();
        assert_eq!(read_worktree_value(&cfg), Some("/abs/path".into()));

        std::fs::write(&cfg, "[core]\nbare = true\n").unwrap();
        assert_eq!(read_worktree_value(&cfg), None);
    }
}
