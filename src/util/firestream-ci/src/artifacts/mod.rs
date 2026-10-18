//! Phase 4 — materialize Nix store outputs into `<rundir>/artifacts/`.
//!
//! Before Phase 4 the binary recorded a manifest entry but left the bytes
//! in the Nix store; consumers had to know the store path and trust that
//! GC wouldn't reap it between record-time and read-time. Now the bytes
//! live in `<rundir>/artifacts/<dest_subdir>/` and the manifest entry
//! gains `nix_store_path`, `sha256`, `size_bytes`, `copy_mode`.
//!
//! Three copy modes:
//! - `Copy` (default): byte-copy the tree. Predictable; `du` is honest;
//!   downstream consumers can mutate.
//! - `Hardlink`: try `hard_link`, fall back per-file to `Copy` on `EXDEV`.
//!   Saves disk on single-FS deployments but silently degrades otherwise.
//! - `Symlink`: a single symlink at the artifact root pointing into the
//!   Nix store. Zero copy; breaks if the store gets GC'd. Power-user opt.
//!
//! Size guard: an artifact whose total bytes exceed `artifacts_max_bytes`
//! is skipped (but still recorded with `skipped_reason="size_cap"`) so
//! we don't multiply image tarballs (~200 MB - 2 GB each) by rundir count.

use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::manifest::{ArtifactKind, Entry, Manifest};
use crate::rundir::RunDir;
use crate::util::fs as ufs;

/// How artifacts are materialised into `<rundir>/artifacts/`. See module
/// doc for tradeoffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ArtifactsFormat {
    /// Byte-copy. Default. Safe across filesystems; consumers can mutate.
    #[default]
    Copy,
    /// Try hard_link, fall back per-file to copy on EXDEV.
    Hardlink,
    /// Single symlink at the artifact root pointing to the Nix store.
    /// Breaks if the store is GC'd. Use only when ephemeral references
    /// are acceptable.
    Symlink,
}

impl ArtifactsFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Hardlink => "hardlink",
            Self::Symlink => "symlink",
        }
    }
}

/// Read `outputs.out` from a `nix-fast-build` per-attr result JSON. The
/// shape:
/// ```json
/// {
///   "results": [
///     {
///       "attr": "...",
///       "outputs": { "out": "/nix/store/..." },
///       "success": true
///     }
///   ]
/// }
/// ```
/// Returns `None` if the file is missing, malformed, or has no successful
/// entry with an `outputs.out`.
pub fn read_store_path(result_file: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(result_file).ok()?;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let arr = doc.get("results").and_then(|v| v.as_array())?;
    for entry in arr {
        // Skip failed entries — their outputs may be partial or absent.
        if entry.get("success").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        if let Some(out) = entry
            .get("outputs")
            .and_then(|o| o.get("out"))
            .and_then(|s| s.as_str())
        {
            if !out.is_empty() {
                return Some(PathBuf::from(out));
            }
        }
    }
    None
}

/// Inputs for materializing one artifact. `store_path` is the only
/// path-shaped input — the function does NOT shell out to Nix. Tests
/// stub it with a fake fixture directory.
pub struct ExportInputs<'a> {
    pub attr: &'a str,
    pub kind: ArtifactKind,
    pub dest_subdir: &'a str,
    pub store_path: &'a Path,
    pub format: ArtifactsFormat,
    pub max_bytes: u64,
}

/// Outcome of a single artifact materialisation. Drives the manifest
/// attrs the caller stamps onto the `Entry`.
pub struct ExportOutcome {
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub copy_mode: ArtifactsFormat,
    pub skipped: bool,
    pub skipped_reason: Option<&'static str>,
}

/// Every `(attr, outputs.out)` pair from a nix-fast-build result file, for
/// entries that succeeded and carry an `out` output.
///
/// The aggregated build phase drives ONE `nix-fast-build` over all of a
/// phase's attrs, so its result file holds one entry per attribute rather than
/// one entry total. [`read_store_path`] answers "the" store path (first
/// success) and is still right for a single-attr invocation; this answers "all
/// of them", which is what an aggregated phase must export.
///
/// `base_attr` is the flake fragment the run was pointed at, and entry attrs
/// are **relative to it**: an aggregate over `packages.x86_64-linux` yields
/// entries named `airflow`, `kafka`, …, while a single-derivation fragment
/// yields ONE entry whose attr is the empty string. Both are rejoined here, so
/// the caller always gets a fully-qualified attribute path — which matters
/// because the export rules match on its LEAF. Without the rejoin a per-attr
/// phase exports everything under the empty leaf.
pub fn result_outputs(result_file: &Path, base_attr: &str) -> Vec<(String, PathBuf)> {
    let Ok(bytes) = std::fs::read(result_file) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(arr) = doc.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in arr {
        if entry.get("success").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        let rel = entry.get("attr").and_then(|v| v.as_str()).unwrap_or("");
        let attr = match (base_attr.is_empty(), rel.is_empty()) {
            (_, true) => base_attr.to_string(),
            (true, false) => rel.to_string(),
            (false, false) => format!("{base_attr}.{rel}"),
        };
        let Some(path) = entry
            .get("outputs")
            .and_then(|o| o.get("out"))
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        // De-duplicate by attribute. nix-fast-build can emit more than one
        // entry per attr in a single result file (observed: three attrs, six
        // entries), and without this each artifact would be copied and hashed
        // once per duplicate.
        if out.iter().any(|(a, _): &(String, PathBuf)| a == &attr) {
            continue;
        }
        out.push((attr, PathBuf::from(path)));
    }
    out
}

/// Export EVERY successful entry of a result file. Used by phases whose
/// profile sets `export_artifacts`, whether aggregated (many entries) or
/// per-attr (one).
///
/// Each entry is routed independently through the profile's `export_targets`,
/// so an aggregate over `packages.<system>` still lands images under
/// `images/<leaf>-<arch>` and charts under `charts/<stem>` exactly as a
/// per-attr run would.
pub async fn export_result_file(
    profile: &crate::profile::Profile,
    base_attr: &str,
    arch: &str,
    rundir: &RunDir,
    result_file: &Path,
    format: ArtifactsFormat,
    max_bytes: u64,
) -> usize {
    let entries = result_outputs(result_file, base_attr);
    for (attr, store_path) in &entries {
        export_store_path(profile, attr, arch, rundir, store_path, format, max_bytes).await;
    }
    entries.len()
}

/// Materialise ONE artifact (the first successful entry of a single-attr
/// result file) and record a manifest entry. Public so the caller can route
/// through one code path regardless of runner.
///
/// On size-cap skip, still records the manifest entry — discoverability
/// matters more than persistence.
pub async fn export_build_artifact(
    profile: &crate::profile::Profile,
    attr: &str,
    arch: &str,
    rundir: &RunDir,
    result_file: &Path,
    format: ArtifactsFormat,
    max_bytes: u64,
) {
    let leaf = attr.rsplit('.').next().unwrap_or(attr);
    let (dest_subdir, kind) = resolve_target(profile, leaf, arch);

    // Resolve Nix store path from the per-attr result file. If the file
    // doesn't carry `outputs.out`, fall back to a record-only entry so
    // the manifest still surfaces the attempted artifact (matches the
    // pre-Phase-4 behaviour).
    let store_path = match read_store_path(result_file) {
        Some(p) => p,
        None => {
            if let Ok(mut m) = Manifest::open_at(rundir.path()).await {
                let entry = Entry::new(kind, &dest_subdir, true)
                    .with_attr("attr", format!(".#{attr}"))
                    .with_attr("dest", dest_subdir);
                let _ = m.record(entry).await;
            }
            return;
        }
    };
    export_store_path(profile, attr, arch, rundir, &store_path, format, max_bytes).await;
}

/// Materialise one already-resolved store path and record its manifest entry.
/// Split out of [`export_build_artifact`] so the aggregated path
/// ([`export_result_file`]) can reuse it without re-reading the result file
/// once per attribute.
pub async fn export_store_path(
    profile: &crate::profile::Profile,
    attr: &str,
    arch: &str,
    rundir: &RunDir,
    store_path: &Path,
    format: ArtifactsFormat,
    max_bytes: u64,
) {
    let leaf = attr.rsplit('.').next().unwrap_or(attr);
    let (dest_subdir, kind) = resolve_target(profile, leaf, arch);
    let store_path = store_path.to_path_buf();

    let inputs = ExportInputs {
        attr,
        kind: kind.clone(),
        dest_subdir: &dest_subdir,
        store_path: &store_path,
        format,
        max_bytes,
    };

    let dest_root = rundir.artifacts_dir().join(&dest_subdir);
    let outcome = match perform_export(&inputs, &dest_root).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                attr = %attr,
                store_path = %store_path.display(),
                error = %e,
                "artifacts: export failed"
            );
            // Record an entry anyway so the failure is discoverable.
            if let Ok(mut m) = Manifest::open_at(rundir.path()).await {
                let entry = Entry::new(kind, &dest_subdir, false)
                    .with_attr("attr", format!(".#{attr}"))
                    .with_attr("dest", dest_subdir.clone())
                    .with_attr("nix_store_path", store_path.to_string_lossy().to_string())
                    .with_attr("error", e);
                let _ = m.record(entry).await;
            }
            return;
        }
    };

    if let Ok(mut m) = Manifest::open_at(rundir.path()).await {
        let mut entry = Entry::new(kind, &dest_subdir, !outcome.skipped)
            .with_attr("attr", format!(".#{attr}"))
            .with_attr("dest", dest_subdir.clone())
            .with_attr("nix_store_path", store_path.to_string_lossy().to_string())
            .with_attr("size_bytes", outcome.size_bytes.to_string())
            .with_attr("copy_mode", outcome.copy_mode.as_str());
        if let Some(sha) = outcome.sha256 {
            entry = entry.with_attr("sha256", sha);
        }
        if outcome.skipped {
            entry = entry.with_attr("skipped", "true");
            if let Some(reason) = outcome.skipped_reason {
                entry = entry.with_attr("skipped_reason", reason);
            }
            entry = entry.with_attr("max_bytes", max_bytes.to_string());
        }
        let _ = m.record(entry).await;
    }
}

/// Public for tests. Performs the size check + copy/hardlink/symlink and
/// (on real copy/hardlink) computes the sha256. Symlink mode skips
/// hashing — the bytes live in the store, not under the rundir.
pub async fn perform_export(
    inputs: &ExportInputs<'_>,
    dest_root: &Path,
) -> Result<ExportOutcome, String> {
    let store_path = inputs.store_path.to_path_buf();

    // Image/container special case: locate the .tar.gz before computing
    // size, so a 1.8 GB directory full of nix-support cruft doesn't fail
    // the size check. `dockerTools.buildLayeredImage` writes the tarball
    // as `$out` directly (a single file, not a dir); `dockerTools.buildImage`
    // wraps it in a directory with sidecar metadata. Handle both shapes.
    let (effective_source, image_single_file) = match inputs.kind {
        ArtifactKind::Image | ArtifactKind::Container => {
            let store_meta = std::fs::symlink_metadata(&store_path)
                .map_err(|e| format!("symlink_metadata {store_path:?}: {e}"))?;
            let is_file = store_meta.file_type().is_file()
                || (store_meta.file_type().is_symlink()
                    && std::fs::metadata(&store_path)
                        .map(|m| m.is_file())
                        .unwrap_or(false));
            if is_file {
                // store_path *is* the tarball.
                (store_path.clone(), true)
            } else {
                match find_largest_tar_gz(&store_path) {
                    Some(p) => (p, true),
                    None => {
                        tracing::warn!(
                            store_path = %store_path.display(),
                            "artifacts: image kind has no .tar.gz; falling back to recursive copy"
                        );
                        (store_path.clone(), false)
                    }
                }
            }
        }
        _ => (store_path.clone(), false),
    };

    let size_bytes = compute_size_bytes(&effective_source)?;

    if size_bytes > inputs.max_bytes {
        tracing::warn!(
            attr = %inputs.attr,
            size_bytes,
            max_bytes = inputs.max_bytes,
            "artifacts: skipping copy — size exceeds cap"
        );
        return Ok(ExportOutcome {
            size_bytes,
            sha256: None,
            copy_mode: inputs.format,
            skipped: true,
            skipped_reason: Some("size_cap"),
        });
    }

    // Compute the materialisation target. For single-file image tarballs
    // we copy the file into `<dest_root>/<filename>` rather than treating
    // the parent directory as the artifact root.
    let dest = if image_single_file {
        // Ensure the dest_subdir exists; the .tar.gz lands inside it.
        let filename = effective_source
            .file_name()
            .ok_or_else(|| "tar.gz has no filename".to_string())?;
        std::fs::create_dir_all(dest_root).map_err(|e| format!("mkdir {dest_root:?}: {e}"))?;
        dest_root.join(filename)
    } else {
        dest_root.to_path_buf()
    };

    // Materialise per format.
    let src_for_copy = effective_source.clone();
    let dest_for_copy = dest.clone();
    let format = inputs.format;
    let is_file = image_single_file;
    let copy_result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        match format {
            ArtifactsFormat::Symlink => {
                // Symlink the artifact root only. If a stale entry exists,
                // remove it first so the rename-ish semantics behave.
                if dest_for_copy.exists() || dest_for_copy.is_symlink() {
                    let _ = std::fs::remove_file(&dest_for_copy);
                    let _ = std::fs::remove_dir_all(&dest_for_copy);
                }
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&src_for_copy, &dest_for_copy)
                        .map_err(|e| format!("symlink: {e}"))
                }
                #[cfg(not(unix))]
                {
                    Err("symlink mode is Unix-only".to_string())
                }
            }
            ArtifactsFormat::Hardlink => {
                if is_file {
                    try_hardlink_or_copy(&src_for_copy, &dest_for_copy)
                } else {
                    hardlink_tree_or_copy(&src_for_copy, &dest_for_copy)
                }
            }
            ArtifactsFormat::Copy => {
                if is_file {
                    if let Some(parent) = dest_for_copy.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("mkdir parent: {e}"))?;
                    }
                    std::fs::copy(&src_for_copy, &dest_for_copy)
                        .map(|_| ())
                        .map_err(|e| format!("copy: {e}"))
                } else {
                    ufs::copy_tree(&src_for_copy, &dest_for_copy).map_err(|e| format!("{e}"))
                }
            }
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?;
    copy_result?;

    // Hash the *destination* bytes — that's what consumers see. For
    // symlink mode we skip; the bytes aren't ours.
    let sha256 = match inputs.format {
        ArtifactsFormat::Symlink => None,
        _ => {
            if is_file {
                tokio::task::spawn_blocking({
                    let p = dest.clone();
                    move || ufs::file_sha256(&p)
                })
                .await
                .ok()
                .and_then(|r| r.ok())
            } else {
                tokio::task::spawn_blocking({
                    let p = dest.clone();
                    move || ufs::dir_sha256(&p)
                })
                .await
                .ok()
                .and_then(|r| r.ok())
            }
        }
    };

    Ok(ExportOutcome {
        size_bytes,
        sha256,
        copy_mode: inputs.format,
        skipped: false,
        skipped_reason: None,
    })
}

fn compute_size_bytes(path: &Path) -> Result<u64, String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("symlink_metadata {path:?}: {e}"))?;
    if meta.file_type().is_file() {
        Ok(meta.len())
    } else if meta.file_type().is_dir() {
        ufs::dir_size_bytes(path).map_err(|e| format!("dir_size_bytes {path:?}: {e}"))
    } else {
        // Symlink at the top — resolve once then size.
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => Ok(m.len()),
            Ok(_) => ufs::dir_size_bytes(path).map_err(|e| format!("{e}")),
            Err(e) => Err(format!("metadata {path:?}: {e}")),
        }
    }
}

/// Recursively search `root` for `.tar.gz` files. Returns the largest one
/// (or `None` if the tree has none). Image-kind store outputs typically
/// have one tarball under `<root>/<name>.tar.gz` or
/// `<root>/nix-support/...`; "largest" is a safe tiebreaker because the
/// extra files are tiny json sidecars.
fn find_largest_tar_gz(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !(name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match &best {
            None => best = Some((entry.path().to_path_buf(), size)),
            Some((_, s)) if size > *s => best = Some((entry.path().to_path_buf(), size)),
            _ => {}
        }
    }
    best.map(|(p, _)| p)
}

fn try_hardlink_or_copy(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
    }
    let _ = std::fs::remove_file(dest);
    match std::fs::hard_link(src, dest) {
        Ok(()) => Ok(()),
        Err(e) if is_exdev(&e) => std::fs::copy(src, dest)
            .map(|_| ())
            .map_err(|e| format!("copy after EXDEV: {e}")),
        Err(e) => Err(format!("hard_link: {e}")),
    }
}

fn hardlink_tree_or_copy(src: &Path, dest: &Path) -> Result<(), String> {
    // File sources short-circuit — same defect as `util::fs::copy_tree` had:
    // walkdir yields one entry whose `strip_prefix(src)` is `""`, so
    // `dest.join("")` is the directory we just created and the hardlink fails
    // with EEXIST/EISDIR. Since sub-tree failures below are only `warn!`ed,
    // that produced a "successful" export with nothing on disk. Reachable
    // whenever a non-image-kind Nix output is a single file (an `sbom.json`,
    // a bare tarball) and `--artifacts-format hardlink` is in play.
    if std::fs::metadata(src).map(|m| m.is_file()).unwrap_or(false) {
        std::fs::create_dir_all(dest).map_err(|e| format!("mkdir dest: {e}"))?;
        let name = src
            .file_name()
            .ok_or_else(|| format!("source has no file name: {}", src.display()))?;
        return try_hardlink_or_copy(src, &dest.join(name));
    }

    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir dest: {e}"))?;
    for entry in walkdir::WalkDir::new(src)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let target = dest.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            let _ = std::fs::create_dir_all(&target);
        } else if ft.is_file() {
            if let Err(e) = try_hardlink_or_copy(entry.path(), &target) {
                tracing::warn!(
                    src = %entry.path().display(),
                    dst = %target.display(),
                    error = %e,
                    "artifacts: hardlink/copy failed; continuing"
                );
            }
        }
        // Symlinks intentionally skipped in hardlink mode — they're rare
        // in image trees and replicating them faithfully without
        // resolving the loop semantics adds complexity for ~0 benefit.
    }
    Ok(())
}

fn is_exdev(e: &std::io::Error) -> bool {
    // raw_os_error 18 == EXDEV on Linux; macOS uses the same value.
    e.raw_os_error() == Some(18)
}

/// Export-target resolver: maps a `build`-phase attr leaf to its
/// `(dest_subdir, ArtifactKind)` inside the rundir.
///
/// Pure lookup into the profile's ordered `export_targets` rules (see
/// [`crate::profile::ExportTarget`] for the matching and templating
/// vocabulary), with `export_default` as the terminal fallback. This is the
/// declarative replacement for ConceptDB's `build_export_target`, preserved
/// at `docs/defaults-reference.rs.txt` — the hardest single case in the
/// inventory, because it combined prefix, suffix and infix matching with a
/// strip-prefix/strip-suffix transform feeding a nested destination path.
/// `tests/profile_fixtures.rs` reproduces all four of its cases from JSON.
///
/// `arch` (and the profile's `nix_system`) are the expansion context; nothing
/// here branches on either.
pub fn resolve_target(
    profile: &crate::profile::Profile,
    leaf: &str,
    arch: &str,
) -> (String, ArtifactKind) {
    let ctx = crate::profile::ExpandCtx {
        system: profile.project.nix_system.clone(),
        arch: arch.to_string(),
        project: profile.project.name.clone(),
    };
    let (dest, kind_str) = profile.resolve_export(leaf, &ctx);
    let kind = match kind_str.as_str() {
        "image" => ArtifactKind::Image,
        "container" => ArtifactKind::Container,
        "binary" => ArtifactKind::Binary,
        "wasm" => ArtifactKind::Wasm,
        "sbom" => ArtifactKind::Sbom,
        "app" => ArtifactKind::App,
        "nix" => ArtifactKind::Nix,
        other => ArtifactKind::Other(other.to_string()),
    };
    (dest, kind)
}

#[cfg(test)]
mod resolve_target_tests {
    use super::*;
    use crate::profile::Profile;

    /// The four `build_export_target` cases from
    /// `docs/defaults-reference.rs.txt`, expressed as profile data and
    /// resolved through the generic engine. The end-to-end version of this
    /// (loading a JSON file for a synthetic non-Firestream project) is
    /// `tests/profile_fixtures.rs`; this one guards the `ArtifactKind`
    /// mapping specifically.
    fn acme_profile() -> Profile {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "project": { "name": "acme", "nix_system": "x86_64-linux", "arch": "x86_64" },
            "export_targets": [
                { "match": { "prefix": "acme-", "contains": "-linux-" },
                  "dest": "{leaf}", "kind": "image" },
                { "match": { "equals": "acme-sbom" }, "dest": "sbom", "kind": "sbom" },
                { "match": { "equals": "acme-cli" }, "dest": "{leaf}-{arch}", "kind": "binary" },
                { "match": { "prefix": "acme-", "suffix": "-wasm" },
                  "strip_prefix": "acme-", "strip_suffix": "-wasm",
                  "dest": "acme-wasm/{stem}", "kind": "wasm" }
            ],
            "export_default": { "dest": "{leaf}", "kind": "binary" }
        }))
        .unwrap()
    }

    #[test]
    fn image_sbom_binary_wasm_and_fallback() {
        let p = acme_profile();
        assert_eq!(
            resolve_target(&p, "acme-server-linux-x86_64", "x86_64"),
            ("acme-server-linux-x86_64".to_string(), ArtifactKind::Image)
        );
        assert_eq!(
            resolve_target(&p, "acme-sbom", "x86_64"),
            ("sbom".to_string(), ArtifactKind::Sbom)
        );
        assert_eq!(
            resolve_target(&p, "acme-cli", "x86_64"),
            ("acme-cli-x86_64".to_string(), ArtifactKind::Binary)
        );
        assert_eq!(
            resolve_target(&p, "acme-portal-wasm", "x86_64"),
            ("acme-wasm/portal".to_string(), ArtifactKind::Wasm)
        );
        // Nothing matches -> export_default.
        assert_eq!(
            resolve_target(&p, "unrelated-thing", "x86_64"),
            ("unrelated-thing".to_string(), ArtifactKind::Binary)
        );
    }

    /// End-to-end regression for the `copy_tree` / `hardlink_tree_or_copy`
    /// file-source bug: an `sbom`-kind export whose store path is a single
    /// file must land at `<dest>/<file-name>` and be recorded as NOT skipped.
    /// Before the fix, both copy modes warned and returned success with an
    /// empty destination directory.
    #[tokio::test]
    async fn file_store_path_exports_under_dest_in_every_copy_mode() {
        for format in [ArtifactsFormat::Copy, ArtifactsFormat::Hardlink] {
            let tmp = tempfile::tempdir().unwrap();
            let store_file = tmp.path().join("sbom.spdx.json");
            std::fs::write(&store_file, b"{\"spdx\":true}").unwrap();

            let dest = tmp.path().join("artifacts/sbom");
            let inputs = ExportInputs {
                attr: "packages.x86_64-linux.manifest",
                kind: ArtifactKind::Sbom,
                dest_subdir: "sbom",
                store_path: &store_file,
                format,
                max_bytes: 10_000_000,
            };
            let outcome = perform_export(&inputs, &dest).await.unwrap();
            assert!(!outcome.skipped, "{format:?}: must not skip");
            assert!(outcome.sha256.is_some(), "{format:?}: must hash the bytes");
            let landed = dest.join("sbom.spdx.json");
            assert!(
                landed.is_file(),
                "{format:?}: expected {}",
                landed.display()
            );
            assert_eq!(std::fs::read(&landed).unwrap(), b"{\"spdx\":true}");
        }
    }

    #[test]
    fn empty_profile_falls_back_to_identity_binary() {
        let p = Profile::default();
        assert_eq!(
            resolve_target(&p, "whatever", "aarch64"),
            ("whatever".to_string(), ArtifactKind::Binary)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_result_file(dir: &Path, store_path: &Path) -> PathBuf {
        let path = dir.join("nix-fast-build-build.fake.json");
        let json = serde_json::json!({
            "results": [
                {
                    "attr": "fake",
                    "success": true,
                    "outputs": { "out": store_path.to_string_lossy() }
                }
            ]
        });
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        path
    }

    #[tokio::test]
    async fn export_records_skipped_when_over_size_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store-out");
        std::fs::create_dir(&store).unwrap();
        // 4 KiB of bytes — small absolute, but with max_bytes=10 it trips.
        std::fs::write(store.join("blob.bin"), vec![0u8; 4096]).unwrap();

        let dest = tmp.path().join("artifacts/out");
        let inputs = ExportInputs {
            attr: "fake",
            kind: ArtifactKind::Binary,
            dest_subdir: "out",
            store_path: &store,
            format: ArtifactsFormat::Copy,
            max_bytes: 10,
        };
        let outcome = perform_export(&inputs, &dest).await.unwrap();
        assert!(outcome.skipped);
        assert_eq!(outcome.skipped_reason, Some("size_cap"));
        assert!(outcome.sha256.is_none());
        // No bytes copied.
        assert!(!dest.join("blob.bin").exists());
    }

    #[tokio::test]
    async fn export_picks_largest_tar_gz_for_image_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store-image");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(store.join("nix-support")).unwrap();
        // Two .tar.gz files — the real one is largest; a tiny stub lives
        // under nix-support/ alongside a json sidecar.
        std::fs::write(store.join("image.tar.gz"), vec![0xAAu8; 1024]).unwrap();
        std::fs::write(store.join("nix-support/aux.tar.gz"), vec![0xBBu8; 32]).unwrap();
        std::fs::write(store.join("nix-support/manifest.json"), b"{}").unwrap();

        let dest_root = tmp.path().join("artifacts/image");
        let inputs = ExportInputs {
            attr: "fake-image",
            kind: ArtifactKind::Image,
            dest_subdir: "image",
            store_path: &store,
            format: ArtifactsFormat::Copy,
            max_bytes: 1024 * 1024,
        };
        let outcome = perform_export(&inputs, &dest_root).await.unwrap();
        assert!(!outcome.skipped);
        // The destination is `<dest_root>/image.tar.gz` — only the tarball,
        // no nix-support/ junk.
        assert!(dest_root.join("image.tar.gz").exists());
        assert!(!dest_root.join("nix-support").exists());
        assert_eq!(outcome.size_bytes, 1024);
        assert!(outcome.sha256.is_some());
    }

    #[tokio::test]
    async fn export_handles_image_kind_when_store_path_is_file_tarball() {
        // dockerTools.buildLayeredImage shape: $out *is* the tarball.
        let tmp = tempfile::tempdir().unwrap();
        let store_tar = tmp.path().join("abc123-image.tar.gz");
        std::fs::write(&store_tar, vec![0xCCu8; 512]).unwrap();

        let dest_root = tmp.path().join("artifacts/demo-server");
        let inputs = ExportInputs {
            attr: "fake-image",
            kind: ArtifactKind::Image,
            dest_subdir: "demo-server",
            store_path: &store_tar,
            format: ArtifactsFormat::Copy,
            max_bytes: 1024 * 1024,
        };
        let outcome = perform_export(&inputs, &dest_root).await.unwrap();
        assert!(!outcome.skipped);
        assert_eq!(outcome.size_bytes, 512);
        // Tarball lands inside the dest_subdir under its original
        // (hash-prefixed) name so consumers can correlate to the store.
        assert!(dest_root.join("abc123-image.tar.gz").exists());
        assert!(outcome.sha256.is_some());
    }

    #[tokio::test]
    async fn read_store_path_extracts_outputs_out() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_store = tmp.path().join("nix-store-out");
        std::fs::create_dir(&fake_store).unwrap();
        let result_file = fake_result_file(tmp.path(), &fake_store);
        let resolved = read_store_path(&result_file).expect("must resolve");
        assert_eq!(resolved, fake_store);
    }
}

/// The aggregated-build reader: one result file, many attrs.
#[cfg(test)]
mod result_outputs_tests {
    use super::*;

    fn write(dir: &Path, body: serde_json::Value) -> PathBuf {
        let p = dir.join("results.json");
        std::fs::write(&p, serde_json::to_vec(&body).unwrap()).unwrap();
        p
    }

    /// The AGGREGATE shape: fragment is `packages.<system>`, entry attrs are
    /// the leaves relative to it.
    #[test]
    fn returns_every_successful_entry_with_an_out_output() {
        let tmp = tempfile::tempdir().unwrap();
        let f = write(
            tmp.path(),
            serde_json::json!({ "results": [
                { "attr": "airflow", "success": true, "type": "build",
                  "duration": 1.0, "error": null, "outputs": { "out": "/nix/store/aaa-airflow" } },
                { "attr": "kafka", "success": true, "type": "build",
                  "duration": 1.0, "error": null, "outputs": { "out": "/nix/store/bbb-kafka" } },
                // Failed: excluded — its outputs may be absent or partial.
                { "attr": "odoo", "success": false, "type": "build",
                  "duration": 1.0, "error": "boom", "outputs": { "out": "/nix/store/ccc-odoo" } },
                // Succeeded but no `out`: nothing to materialise.
                { "attr": "meta", "success": true, "type": "build",
                  "duration": 1.0, "error": null },
                // Duplicate of an earlier attr — nix-fast-build emits these;
                // exporting twice would copy and hash the same bytes twice.
                { "attr": "airflow", "success": true, "type": "build",
                  "duration": 1.0, "error": null, "outputs": { "out": "/nix/store/aaa-airflow" } },
            ]}),
        );
        let got = result_outputs(&f, "packages.x86_64-linux");
        assert_eq!(
            got,
            vec![
                ("packages.x86_64-linux.airflow".to_string(), PathBuf::from("/nix/store/aaa-airflow")),
                ("packages.x86_64-linux.kafka".to_string(), PathBuf::from("/nix/store/bbb-kafka")),
            ]
        );
        // `read_store_path` answers "the" path (first success) and is still
        // right for a single-attr invocation — the two must not disagree about
        // which entries count.
        assert_eq!(read_store_path(&f), Some(PathBuf::from("/nix/store/aaa-airflow")));
    }

    /// The PER-ATTR shape: the fragment already names a single derivation, so
    /// nix-eval-jobs emits one entry with an EMPTY relative attr. Without the
    /// rejoin the export leaf would be `""` and every artifact in a per-attr
    /// phase would land in the same catch-all destination — which is exactly
    /// what the first Phase-6 end-to-end run produced (`dest: "profile/"`,
    /// `attr: ".#"`).
    #[test]
    fn a_single_derivation_fragment_keeps_its_own_attr_name() {
        let tmp = tempfile::tempdir().unwrap();
        let f = write(
            tmp.path(),
            serde_json::json!({ "results": [
                { "attr": "", "success": true, "type": "build", "duration": 1.0,
                  "error": null, "outputs": { "out": "/nix/store/zzz-manifest" } },
            ]}),
        );
        assert_eq!(
            result_outputs(&f, "packages.x86_64-linux.manifest"),
            vec![(
                "packages.x86_64-linux.manifest".to_string(),
                PathBuf::from("/nix/store/zzz-manifest")
            )]
        );
    }

    #[test]
    fn a_missing_or_garbled_result_file_is_empty_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(result_outputs(&tmp.path().join("nope.json"), "x").is_empty());
        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, b"{ not json").unwrap();
        assert!(result_outputs(&bad, "x").is_empty());
    }

    /// Regression guard for the export-destination collision the Phase-6
    /// review found: a rule writing to `charts` and another writing to
    /// `charts/<stem>` share a subtree, so two concurrently-finishing exports
    /// interleave inside it. Destinations from distinct rules must be
    /// non-nested. Expressed generically — the fix lives in profile data.
    #[test]
    fn export_destinations_must_not_nest() {
        let profile: crate::profile::Profile = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "project": { "name": "acme", "nix_system": "x86_64-linux", "arch": "x86_64" },
            "export_targets": [
                { "match": { "equals": "bundle" }, "dest": "charts-bundle", "kind": "nix" },
                { "match": { "suffix": "-chart" }, "strip_suffix": "-chart",
                  "dest": "charts/{stem}", "kind": "nix" },
                { "match": {}, "dest": "images/{leaf}-{arch}", "kind": "image" },
            ],
        }))
        .unwrap();
        profile.validate().unwrap();
        let dests: Vec<String> = ["bundle", "airflow-chart", "kafka"]
            .iter()
            .map(|l| resolve_target(&profile, l, "x86_64").0)
            .collect();
        assert_eq!(dests, vec!["charts-bundle", "charts/airflow", "images/kafka-x86_64"]);
        for (i, a) in dests.iter().enumerate() {
            for (j, b) in dests.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    !b.starts_with(&format!("{a}/")),
                    "export dest {b:?} nests inside {a:?}; concurrent exports \
                     would interleave in the same subtree"
                );
            }
        }
    }
}
