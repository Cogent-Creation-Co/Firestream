//! Symlink-divergence walk. Given a logical path (caller-supplied, may
//! traverse symlinks) and its physical canonicalization, find the closest
//! ancestor pair where the two diverge — that's the mount point libgit2
//! needs.
//!
//! Algorithm (mirrors the bash, lines 92-100 of `build/_common.sh`):
//!
//! ```text
//! while basename(logical) == basename(physical):
//!     if either is "/": break
//!     logical = dirname(logical)
//!     physical = dirname(physical)
//! ```
//!
//! Once that loop exits, `logical` and `physical` are the closest matching
//! ancestor (last common name) — if they differ, that's the divergence
//! point. The bash then emits a mount of `physical_repo : logical_repo`
//! pair so libgit2 can resolve the symlink-traversed string to a real
//! directory inside the container.

use std::path::{Path, PathBuf};

/// Where a logical path and its canonical (physical) target diverge.
///
/// If `logical == physical`, no symlink is involved.
/// Otherwise `divergent_logical` and `divergent_physical` name the closest
/// common-named ancestor where the two parents differ. Callers wanting a
/// container mount typically issue both directions:
///   * `physical_root → logical_root` so libgit2's string-concat lookup
///     finds a real dir.
///   * `physical_root → physical_root` is implicit (the workspace bind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergencePoint {
    /// The original logical path (caller's view).
    pub logical: PathBuf,
    /// The same path canonicalized.
    pub physical: PathBuf,
    /// True if the two paths differ.
    pub diverges: bool,
    /// Closest ancestor in the logical view where the two start agreeing
    /// (or `/` if no agreement). Only meaningful when `diverges` is true.
    pub divergent_logical: PathBuf,
    /// Mirror of `divergent_logical` in the physical view.
    pub divergent_physical: PathBuf,
}

/// Run the tail-walk. Both paths must be absolute; non-absolute input is
/// returned as a non-diverging point (degenerate case the bash short-
/// circuits at line 89: `[[ "$abs_portion" != "$resolved" ]]`).
pub fn divergence_walk(logical: &Path, physical: &Path) -> DivergencePoint {
    if logical == physical {
        return DivergencePoint {
            logical: logical.to_path_buf(),
            physical: physical.to_path_buf(),
            diverges: false,
            divergent_logical: logical.to_path_buf(),
            divergent_physical: physical.to_path_buf(),
        };
    }

    let mut l = logical.to_path_buf();
    let mut p = physical.to_path_buf();

    // Strip the trailing common components by name. The bash uses
    // basename, which on `/foo/bar/` yields "bar" — i.e. trailing slash
    // is irrelevant. Rust's `file_name` is the equivalent.
    loop {
        let l_name = l.file_name().map(|n| n.to_os_string());
        let p_name = p.file_name().map(|n| n.to_os_string());
        match (l_name, p_name) {
            (Some(a), Some(b)) if a == b => {
                if let (Some(lp), Some(pp)) = (
                    l.parent().map(Path::to_path_buf),
                    p.parent().map(Path::to_path_buf),
                ) {
                    if lp.as_os_str().is_empty() || pp.as_os_str().is_empty() {
                        break;
                    }
                    l = lp;
                    p = pp;
                } else {
                    break;
                }
            }
            _ => break,
        }
        if l == Path::new("/") || p == Path::new("/") {
            break;
        }
    }

    DivergencePoint {
        logical: logical.to_path_buf(),
        physical: physical.to_path_buf(),
        diverges: true,
        divergent_logical: l,
        divergent_physical: p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn identical_paths_do_not_diverge() {
        let d = divergence_walk(&p("/a/b/c"), &p("/a/b/c"));
        assert!(!d.diverges);
    }

    #[test]
    fn shared_suffix_strips_to_divergent_ancestor() {
        // logical : /Users/x/Dev/repo/sub/foo
        // physical: /Volumes/Disk/Dev/repo/sub/foo
        // tail-walk strips "foo", "sub", "repo", "Dev" → diverges at
        // /Users/x  vs  /Volumes/Disk
        let d = divergence_walk(
            &p("/Users/x/Dev/repo/sub/foo"),
            &p("/Volumes/Disk/Dev/repo/sub/foo"),
        );
        assert!(d.diverges);
        assert_eq!(d.divergent_logical, p("/Users/x"));
        assert_eq!(d.divergent_physical, p("/Volumes/Disk"));
    }

    #[test]
    fn divergence_at_root_terminates() {
        // logical : /a/b/c
        // physical: /x/b/c
        // Strips "c", "b", then "a" vs "x" mismatch → divergent.
        let d = divergence_walk(&p("/a/b/c"), &p("/x/b/c"));
        assert!(d.diverges);
        assert_eq!(d.divergent_logical, p("/a"));
        assert_eq!(d.divergent_physical, p("/x"));
    }

    #[test]
    fn diverges_immediately_when_basenames_differ() {
        let d = divergence_walk(&p("/a/b/foo"), &p("/a/b/bar"));
        assert!(d.diverges);
        assert_eq!(d.divergent_logical, p("/a/b/foo"));
        assert_eq!(d.divergent_physical, p("/a/b/bar"));
    }

    #[test]
    fn trailing_slash_irrelevant() {
        let d = divergence_walk(&p("/a/b/c/"), &p("/a/b/c"));
        assert!(!d.diverges);
    }
}
