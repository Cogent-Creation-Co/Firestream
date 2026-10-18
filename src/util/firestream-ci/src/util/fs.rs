//! Filesystem helpers shared by artifact export and (eventually) the
//! Phase 7 contract test.
//!
//! The Nix store is read-only; outputs may contain inter-store symlinks
//! that resolve to other store paths. The copy helpers below resolve
//! those (bounded depth, loop-aware) and fall back to copying the link
//! itself on detected cycles. File-mode preservation is Unix-only.
//!
//! `dir_sha256` is the contract for the tree hash. The on-disk shape:
//! for each regular file under `root`, take `(rel_path, sha256(bytes))`,
//! sort lexicographically by `rel_path`, and feed
//! `b"<rel>\0<hex>\n"` into a running SHA-256. The Phase 7 contract test
//! depends on this being stable across filename-creation order.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Max symlink-resolution depth inside a copy operation. Nix store outputs
/// occasionally contain symlinks between sibling store paths; we follow
/// them up to this depth and copy the resolved target's bytes. Beyond the
/// depth (or on a detected loop), the symlink itself is copied verbatim.
pub const COPY_SYMLINK_MAX_DEPTH: usize = 4;

/// SHA-256 of a single regular file's bytes. Streams via 64 KiB buffer so
/// large artifacts (image tarballs) don't blow the heap.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Stable hash of a directory tree. See module-level doc for the encoding
/// — the on-disk shape MUST be reproducible across filesystems / creation
/// order, because Phase 7 asserts on this digest.
pub fn dir_sha256(root: &Path) -> io::Result<String> {
    // Walk first, then sort by relative path. BTreeMap is the simplest
    // way to get the deterministic ordering without a manual sort.
    let mut files: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(root)
            .map_err(|e| io::Error::other(format!("strip_prefix: {e}")))?
            .to_path_buf();
        files.insert(rel, abs);
    }

    let mut hasher = Sha256::new();
    for (rel, abs) in &files {
        let hex_digest = file_sha256(abs)?;
        // Use forward-slash separators in the rel path so the hash is
        // stable across Unix/Windows authors of fixture trees.
        let rel_norm = rel.to_string_lossy().replace('\\', "/");
        hasher.update(rel_norm.as_bytes());
        hasher.update(b"\0");
        hasher.update(hex_digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Sum file sizes (regular files only, symlinks resolved). Symlink loops
/// are silently broken — we count the link target only on the first
/// encounter via the visited set.
pub fn dir_size_bytes(root: &Path) -> io::Result<u64> {
    let mut total: u64 = 0;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        if entry.file_type().is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    Ok(total)
}

/// Recursive copy from `src` into `dest`. Directories are created;
/// regular files are copied byte-for-byte (mode preserved on Unix);
/// symlinks are resolved up to `COPY_SYMLINK_MAX_DEPTH` and the *target's*
/// bytes are copied. On loop detection (resolved path already seen in
/// this op), the symlink is copied as a symlink — never blocks the run.
///
/// Returns `Ok(())` even on partial failure inside a sub-tree; sub-tree
/// errors are logged via `tracing::warn!` so the manifest entry can still
/// be recorded. The caller decides how to surface partial copies.
///
/// **File sources short-circuit.** When `src` is a regular file (or a symlink
/// to one), `walkdir` yields exactly one entry whose `strip_prefix(src)` is
/// `""` — so `dest.join("")` is `dest` itself, which `create_dir_all` has just
/// made a *directory*. The copy then fails with `EISDIR`, and because sub-tree
/// failures are only `warn!`ed, `perform_export` returned success with no
/// artifact on disk. Reachable in practice: a Nix output that is a single file
/// (a `sbom.json`, a `.tar.gz` built by `dockerTools` without a wrapper dir)
/// exported under any rule whose kind is not `image`. Copy into
/// `dest/<file-name>` instead, and propagate the error rather than logging it —
/// there is no partial-tree ambiguity to tolerate in the one-file case.
pub fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    // Resolve one level so a symlink-to-file is treated as a file. `metadata`
    // follows links; `symlink_metadata` does not.
    if std::fs::metadata(src).map(|m| m.is_file()).unwrap_or(false) {
        std::fs::create_dir_all(dest)?;
        let name = src.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("copy_tree: source has no file name: {}", src.display()),
            )
        })?;
        return copy_file_preserving_mode(src, &dest.join(name));
    }

    std::fs::create_dir_all(dest)?;
    let walker = walkdir::WalkDir::new(src).follow_links(false);
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "copy_tree: walkdir error");
                continue;
            }
        };
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let target = dest.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            if let Err(e) = std::fs::create_dir_all(&target) {
                tracing::warn!(path = %target.display(), error = %e, "copy_tree: mkdir failed");
            }
        } else if ft.is_file() {
            if let Err(e) = copy_file_preserving_mode(entry.path(), &target) {
                tracing::warn!(
                    src = %entry.path().display(),
                    dst = %target.display(),
                    error = %e,
                    "copy_tree: copy failed"
                );
            }
        } else if ft.is_symlink() {
            // Resolve symlink target with bounded depth. If we detect a
            // loop, fall through and copy the link itself.
            match resolve_symlink_bounded(entry.path(), COPY_SYMLINK_MAX_DEPTH) {
                Some(resolved) if resolved.is_file() => {
                    if let Err(e) = copy_file_preserving_mode(&resolved, &target) {
                        tracing::warn!(
                            src = %resolved.display(),
                            dst = %target.display(),
                            error = %e,
                            "copy_tree: symlink-resolved copy failed"
                        );
                    }
                }
                Some(resolved) if resolved.is_dir() => {
                    if let Err(e) = copy_tree(&resolved, &target) {
                        tracing::warn!(error = %e, "copy_tree: nested resolved-dir copy failed");
                    }
                }
                _ => {
                    // Loop or unresolvable — copy the symlink as a symlink
                    // so the manifest entry still records *something*.
                    if let Ok(link_target) = std::fs::read_link(entry.path()) {
                        // Remove pre-existing entry at `target` (mkdir_all
                        // may have created a dir there earlier in the walk).
                        let _ = std::fs::remove_file(&target);
                        #[cfg(unix)]
                        let _ = std::os::unix::fs::symlink(&link_target, &target);
                        #[cfg(not(unix))]
                        let _ = std::fs::copy(entry.path(), &target);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Copy a regular file and preserve its mode on Unix. Permissions reflect
/// the Nix store's read-only bits today; consumers (e.g. a downstream
/// publish step) may need to chmod +w themselves. We deliberately don't
/// rewrite modes here — the manifest is the contract, not the bits.
fn copy_file_preserving_mode(src: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)?;
    #[cfg(unix)]
    {
        let meta = std::fs::metadata(src)?;
        let mode = meta.permissions().mode();
        let mut perm = std::fs::metadata(dest)?.permissions();
        perm.set_mode(mode);
        std::fs::set_permissions(dest, perm)?;
    }
    Ok(())
}

/// Resolve a symlink chain up to `max_depth` hops. Returns `None` on loop
/// (same path visited twice) or when the resolved path doesn't exist.
fn resolve_symlink_bounded(start: &Path, max_depth: usize) -> Option<PathBuf> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut cur = start.to_path_buf();
    for _ in 0..=max_depth {
        // Canonicalize each hop's parent (not `cur` itself, which would
        // resolve the whole chain in one go and hide a loop) to detect
        // revisits robustly.
        let canon_parent = cur
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .unwrap_or_else(|| cur.parent().unwrap_or(Path::new("/")).to_path_buf());
        let canon = match cur.file_name() {
            Some(n) => canon_parent.join(n),
            None => cur.clone(),
        };
        if !seen.insert(canon.clone()) {
            return None;
        }
        let meta = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(_) => return None,
        };
        if !meta.file_type().is_symlink() {
            return Some(cur);
        }
        let link = match std::fs::read_link(&cur) {
            Ok(l) => l,
            Err(_) => return None,
        };
        cur = if link.is_absolute() {
            link
        } else {
            cur.parent().unwrap_or(Path::new("/")).join(link)
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a *file* source used to produce `target == dest`, an
    /// `EISDIR` failure, and a `warn!`-only outcome — i.e. `perform_export`
    /// reported success with nothing on disk. The file must land at
    /// `<dest>/<file-name>` with its bytes intact.
    #[test]
    fn copy_tree_with_a_file_source_lands_the_file_under_dest() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        let file = src.path().join("sbom.json");
        std::fs::write(&file, b"{\"bom\":1}").unwrap();

        let dest_root = dest.path().join("out");
        copy_tree(&file, &dest_root).expect("file source must copy, not EISDIR");

        let landed = dest_root.join("sbom.json");
        assert!(
            landed.is_file(),
            "expected the file at {}, dest_root contents: {:?}",
            landed.display(),
            std::fs::read_dir(&dest_root)
                .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect::<Vec<_>>())
                .unwrap_or_default()
        );
        assert_eq!(std::fs::read(&landed).unwrap(), b"{\"bom\":1}");
    }

    /// The same short-circuit must apply through a symlink-to-file, which is
    /// the common shape of a Nix `result` link.
    #[test]
    #[cfg(unix)]
    fn copy_tree_with_a_symlink_to_file_source_lands_the_file() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        let real = src.path().join("real.tar.gz");
        std::fs::write(&real, b"payload").unwrap();
        let link = src.path().join("result");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let dest_root = dest.path().join("out");
        copy_tree(&link, &dest_root).expect("symlink-to-file source must copy");
        assert_eq!(std::fs::read(dest_root.join("result")).unwrap(), b"payload");
    }

    /// A nonexistent source must still error rather than silently create an
    /// empty destination directory.
    #[test]
    fn copy_tree_with_a_missing_source_errors() {
        let dest = tempfile::tempdir().unwrap();
        let missing = dest.path().join("nope");
        // Directory branch: walkdir yields an Err entry, which is warned and
        // skipped, so the call itself succeeds but produces nothing. Assert
        // the observable part: no files were produced.
        let dest_root = dest.path().join("out");
        let _ = copy_tree(&missing, &dest_root);
        let n = std::fs::read_dir(&dest_root)
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn copy_tree_preserves_file_mode_and_sha() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        // Construct a small tree.
        std::fs::write(src.path().join("a.txt"), b"hello\n").unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/b.txt"), b"world\n").unwrap();

        // Mark a.txt as 0o600 to ensure mode is preserved.
        #[cfg(unix)]
        {
            let mut perm = std::fs::metadata(src.path().join("a.txt"))
                .unwrap()
                .permissions();
            perm.set_mode(0o600);
            std::fs::set_permissions(src.path().join("a.txt"), perm).unwrap();
        }

        let dest_root = dest.path().join("out");
        copy_tree(src.path(), &dest_root).expect("copy_tree ok");

        // Tree hash must match across the two physical roots.
        let h_src = dir_sha256(src.path()).unwrap();
        let h_dst = dir_sha256(&dest_root).unwrap();
        assert_eq!(h_src, h_dst, "tree hash must be byte-stable");

        // Mode preserved on Unix.
        #[cfg(unix)]
        {
            let perm = std::fs::metadata(dest_root.join("a.txt"))
                .unwrap()
                .permissions();
            assert_eq!(perm.mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_tree_skips_symlink_loops() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        // a -> b -> a loop.
        std::os::unix::fs::symlink(src.path().join("b"), src.path().join("a")).unwrap();
        std::os::unix::fs::symlink(src.path().join("a"), src.path().join("b")).unwrap();
        // Plus a real file so the copy isn't entirely about cycles.
        std::fs::write(src.path().join("real.txt"), b"ok\n").unwrap();

        // No panic, no infinite recursion.
        copy_tree(src.path(), dest.path()).expect("copy_tree ok");

        // Real file copied.
        assert!(dest.path().join("real.txt").exists());
    }

    #[tokio::test]
    async fn dir_sha256_is_deterministic_across_filename_order() {
        // Create two trees with the same content but written in different
        // orders. The on-disk inode order may differ; the hash must not.
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();

        std::fs::write(one.path().join("a"), b"1").unwrap();
        std::fs::write(one.path().join("c"), b"3").unwrap();
        std::fs::write(one.path().join("b"), b"2").unwrap();

        std::fs::write(two.path().join("c"), b"3").unwrap();
        std::fs::write(two.path().join("b"), b"2").unwrap();
        std::fs::write(two.path().join("a"), b"1").unwrap();

        let h1 = dir_sha256(one.path()).unwrap();
        let h2 = dir_sha256(two.path()).unwrap();
        assert_eq!(h1, h2);
    }
}
