//! Preload Nix-built container images into a target cluster's containerd.
//!
//! The Firestream charts ship Nix-built `firestream-*` images that ONLY
//! exist as flake derivations on the developer's machine. They reference
//! registry-less repositories (`firestream-<app>:<tag>`) with
//! `pullPolicy: IfNotPresent`, so unless the image is side-loaded into the
//! target cluster's containerd BEFORE the helm deploy, every pod will
//! `ImagePullBackOff` trying to reach a registry that doesn't have them.
//!
//! Two materialisation steps:
//!
//! 1. Build the image into the host docker daemon's image store via
//!    `nix run .#<flake-attr>-image -- --load` (shared by all targets).
//! 2. Side-load into the target cluster's containerd. The mechanism
//!    depends on [`ImportTarget`]:
//!    - [`ImportTarget::K3d`] → `k3d image import <repo>:<tag> -c <cluster>`.
//!    - [`ImportTarget::HostK3s`] → `docker save <ref> -o <tmp.tar>` then
//!      import via `ctr -n k8s.io images import <tmp.tar>` against the host
//!      k3s containerd socket (overridable — see below).
//!
//! # Image-name resolution (host-k3s)
//!
//! A registry-less ref `firestream-postgresql:17` is normalised by
//! containerd/kubelet to `docker.io/library/firestream-postgresql:17`.
//! `docker save firestream-postgresql:17` writes that fully-qualified repo
//! tag into the tarball, and `ctr images import` preserves it — so the
//! image lands under `docker.io/library/firestream-postgresql:17`, exactly
//! the name the kubelet looks up. No `ctr images tag` step is required
//! (verified live against k3s 1.31 / containerd).
//!
//! # Skipping
//!
//! This module does NOT read any env var to decide whether to run. The
//! caller owns the skip decision (e.g. the e2e harness honours
//! `FIRESTREAM_E2E_K8S_PRELOAD=0`; the CLI honours `--no-preload` /
//! `FIRESTREAM_APP_PRELOAD=0`). Call [`preload_images`] only when preload
//! is wanted.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use firestream_charts::ChartManifest;
use tracing::warn;

/// Default host-k3s containerd socket. Used when the operator hasn't set
/// `FIRESTREAM_K8S_IMPORT_CMD`. The current user must be able to talk to
/// this socket (member of the socket's group, or root); on a typical
/// rootless-but-grouped k3s install `ctr -a <sock> -n k8s.io images ls`
/// works without sudo.
const DEFAULT_K3S_CONTAINERD_SOCK: &str = "/run/k3s/containerd/containerd.sock";

/// Where to side-load the docker-materialised image after the shared
/// `nix run … --load` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// k3d cluster: `k3d image import <ref> -c <cluster>`.
    K3d { cluster: String },
    /// Ambient host k3s: `docker save` → `ctr -n k8s.io images import`.
    HostK3s,
}

/// One image that needs to be materialised + imported. Fields are:
///
/// - `flake_attr` — fully-qualified flake-app reference, e.g.
///   `.#airflow-image`. Derived from the image repository (NOT the slot
///   key — slot keys like `hub`/`proxy`/`singleuser` would map to
///   non-existent flake apps for multi-slot charts).
/// - `repo`       — image repository as it appears in `manifest.images`
///   (e.g. `firestream-airflow`). Used to form the import ref.
/// - `tag`        — image tag (e.g. `3.0.3`). Used to form the import ref.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PreloadTarget {
    flake_attr: String,
    repo: String,
    tag: String,
}

/// Pure helper extracted from [`preload_images`] for testability.
///
/// Walk the manifest's `images` map and derive the set of distinct
/// preload targets. We:
///
/// - Skip slots without a repository or tag (warn on the tag case).
/// - Skip Bitnami-shaped repositories (defence in depth; post-Phase-B
///   there should be none).
/// - Derive the flake attr via `flake_attr_stem(repo)` — this maps
///   `firestream-jupyterhub` → `.#jupyterhub-image`,
///   `firestream-os-shell` → `.#os-shell-image`, etc. The slot key
///   (`hub`, `proxy`, `singleuser`, `auxiliaryImage`, …) is NOT used —
///   multi-slot charts like jupyterhub have 3+ slots pointing at the
///   same flake app, and slot keys don't always match flake attrs.
/// - De-duplicate on `(flake_attr, repo, tag)` so the same image isn't
///   loaded multiple times when several slots reference it.
fn derive_preload_targets(manifest: &ChartManifest) -> HashSet<PreloadTarget> {
    let mut out: HashSet<PreloadTarget> = HashSet::new();
    for (slot, image) in &manifest.images {
        let repo = match image.repository.as_deref() {
            Some(r) if !r.is_empty() => r,
            _ => {
                // Slot without a repo override: nothing to materialise.
                continue;
            }
        };
        if is_bitnami(repo) {
            continue;
        }
        let tag = match image.tag.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                warn!(
                    chart = %manifest.name,
                    slot,
                    repo,
                    "image slot has repository but no tag; skipping preload"
                );
                continue;
            }
        };
        let flake_attr = format!(".#{}-image", flake_attr_stem(repo));
        out.insert(PreloadTarget {
            flake_attr,
            repo: repo.to_string(),
            tag,
        });
    }
    out
}

/// Preload every non-Bitnami image referenced by `manifest` into the
/// cluster designated by `target`. No-op when:
///
/// - `manifest.images` is empty.
/// - Every image in the manifest is Bitnami-shaped (no current chart hits
///   this path post-Phase-B, but the guard remains).
///
/// For each distinct non-Bitnami image we:
///
/// 1. Run `nix run <flake-attr> -- --load` synchronously (stdio inherited
///    so the user sees progress; this can take minutes the first time and
///    is cached on subsequent runs). The flake attr is derived from the
///    image repository, not the slot key.
/// 2. Side-load into `target`'s containerd (see [`ImportTarget`]).
///
/// Distinct images are de-duplicated upfront — jupyterhub references
/// `firestream-jupyterhub:5.3.0` from three slots (`hub`, `proxy`,
/// `singleuser`); we run `nix run .#jupyterhub-image` exactly once.
///
/// Returns `Err` with context on any failure.
pub fn preload_images(target: &ImportTarget, manifest: &ChartManifest) -> Result<()> {
    let targets = derive_preload_targets(manifest);
    if targets.is_empty() {
        eprintln!(
            "[preload] no non-bitnami images for chart `{}`; skipping",
            manifest.name
        );
        return Ok(());
    }

    for target_image in targets {
        let PreloadTarget {
            flake_attr,
            repo,
            tag,
        } = target_image;
        eprintln!(
            "[preload] nix run {} -- --load (for {}:{})",
            flake_attr, repo, tag
        );
        let status = Command::new("nix")
            .args(["run", &flake_attr, "--", "--load"])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("spawn nix run {}", flake_attr))?;
        if !status.success() {
            bail!(
                "nix run {} -- --load failed with status {} (chart `{}` repo `{}`)",
                flake_attr,
                status,
                manifest.name,
                repo
            );
        }

        let image_ref = format!("{}:{}", repo, tag);
        import_into_cluster(target, &image_ref)
            .with_context(|| format!("import {} into {:?}", image_ref, target))?;
    }

    Ok(())
}

/// `firestream-e2e-k8s` compatibility shim. The harness still calls
/// `preload_images_for(handle, manifest)` with a k3d [`ClusterHandle`].
/// Delegates to [`preload_images`] with an [`ImportTarget::K3d`] derived
/// from the handle name. The harness's own `FIRESTREAM_E2E_K8S_PRELOAD=0`
/// gate is applied by the caller before this runs.
pub fn preload_images_for(
    handle: &super::cluster::ClusterHandle,
    manifest: &ChartManifest,
) -> Result<()> {
    preload_images(
        &ImportTarget::K3d {
            cluster: handle.name.clone(),
        },
        manifest,
    )
}

/// Side-load a docker-materialised `image_ref` into the target cluster's
/// containerd.
fn import_into_cluster(target: &ImportTarget, image_ref: &str) -> Result<()> {
    match target {
        ImportTarget::K3d { cluster } => import_k3d(cluster, image_ref),
        ImportTarget::HostK3s => import_host_k3s(image_ref),
    }
}

/// `k3d image import <ref> -c <cluster>`.
fn import_k3d(cluster: &str, image_ref: &str) -> Result<()> {
    eprintln!("[preload] k3d image import {} -c {}", image_ref, cluster);
    let status = Command::new("k3d")
        .args(["image", "import", image_ref, "-c", cluster])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawn k3d image import {}", image_ref))?;
    if !status.success() {
        bail!(
            "k3d image import {} -c {} failed with status {}",
            image_ref,
            cluster,
            status
        );
    }
    Ok(())
}

/// Host-k3s side-load: `docker save <ref> -o <tmp.tar>` then import the
/// tar into the k8s.io containerd namespace.
///
/// The import command is resolved as:
/// - if `FIRESTREAM_K8S_IMPORT_CMD` is set → run `<that cmd> <tarfile>`
///   (the cmd is split on whitespace; the tarfile is appended as the final
///   arg). This is the privilege-handoff seam for installs where the
///   current user can't reach the containerd socket directly (e.g.
///   `sudo ctr -n k8s.io images import`, or a wrapper script).
/// - else → `ctr -a /run/k3s/containerd/containerd.sock -n k8s.io images
///   import <tarfile>`.
///
/// The temp tar is always cleaned up (even on the error path).
fn import_host_k3s(image_ref: &str) -> Result<()> {
    let tar = tempfile_path(image_ref)?;
    let result = import_host_k3s_inner(image_ref, &tar);
    // Always clean up the (large) tar, regardless of success.
    let _ = std::fs::remove_file(&tar);
    result
}

fn import_host_k3s_inner(image_ref: &str, tar: &PathBuf) -> Result<()> {
    eprintln!("[preload] docker save {} -o {}", image_ref, tar.display());
    let status = Command::new("docker")
        .arg("save")
        .arg(image_ref)
        .arg("-o")
        .arg(tar)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawn docker save {}", image_ref))?;
    if !status.success() {
        bail!("docker save {} failed with status {}", image_ref, status);
    }

    let (program, mut args) = host_import_command();
    args.push(tar.to_string_lossy().to_string());
    eprintln!(
        "[preload] host-k3s import: {} {}",
        program,
        args.join(" ")
    );
    let status = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawn host-k3s import command `{}`", program))?;
    if !status.success() {
        bail!(
            "host-k3s image import of `{}` failed (command: `{} {}`, status {}).\n\
             The default importer talks to the k3s containerd socket at {} and requires \
             the current user to be able to reach it (member of its group, or root). \
             If your install needs elevated access or a different socket, set \
             FIRESTREAM_K8S_IMPORT_CMD to a command that imports a docker-save tar into \
             the `k8s.io` containerd namespace (the tar path is appended as the final arg), \
             e.g. `sudo ctr -n k8s.io images import`.",
            image_ref,
            program,
            args.join(" "),
            status,
            DEFAULT_K3S_CONTAINERD_SOCK
        );
    }
    Ok(())
}

/// Resolve `(program, leading_args)` for the host-k3s import step. The tar
/// path is appended by the caller.
fn host_import_command() -> (String, Vec<String>) {
    if let Ok(cmd) = std::env::var("FIRESTREAM_K8S_IMPORT_CMD") {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            let mut parts = trimmed.split_whitespace().map(|s| s.to_string());
            if let Some(program) = parts.next() {
                let args: Vec<String> = parts.collect();
                return (program, args);
            }
        }
    }
    (
        "ctr".to_string(),
        vec![
            "-a".to_string(),
            DEFAULT_K3S_CONTAINERD_SOCK.to_string(),
            "-n".to_string(),
            "k8s.io".to_string(),
            "images".to_string(),
            "import".to_string(),
        ],
    )
}

/// A unique temp-tar path for a given image ref. We don't use the `tempfile`
/// crate to avoid adding a dep; a pid+nanos-suffixed path in the system temp
/// dir is sufficient (the importer reads it immediately and we delete it).
fn tempfile_path(image_ref: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let safe: String = image_ref
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = format!("firestream-preload-{}-{}-{}.tar", safe, std::process::id(), nanos);
    Ok(std::env::temp_dir().join(name))
}

/// Heuristic: treat any repo whose last `/`-segment is rooted at `bitnami/`
/// (or that contains `/bitnami/`, or starts with `bitnami/`) as a Bitnami
/// image that the cluster will pull from upstream on demand.
fn is_bitnami(repo: &str) -> bool {
    repo.starts_with("bitnami/")
        || repo.starts_with("docker.io/bitnami/")
        || repo.contains("/bitnami/")
}

/// Derive the Nix flake attribute base from an image repository name.
///
/// Two transformations:
///
/// 1. Strip any registry-namespace prefix (`myorg/firestream-airflow` →
///    `firestream-airflow`). Last `/`-segment wins.
/// 2. Strip the `firestream-` prefix (`firestream-airflow` → `airflow`,
///    `firestream-os-shell` → `os-shell`). The flake apps are registered
///    as `<short-name>-image` in `nix/flake-modules/charts/*.nix` and
///    `nix/flake-modules/containers/*.nix` — they do NOT carry the
///    `firestream-` prefix.
///
/// Multi-slot charts (jupyterhub: `hub`/`proxy`/`singleuser` all point at
/// `firestream-jupyterhub`, plus `auxiliaryImage` at `firestream-os-shell`)
/// rely on this — the slot key is unreliable as a flake-attr source.
fn flake_attr_stem(repo: &str) -> &str {
    let after_namespace = repo.rsplit('/').next().unwrap_or(repo);
    after_namespace
        .strip_prefix("firestream-")
        .unwrap_or(after_namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firestream_charts::ImageSlot;
    use std::collections::BTreeMap;

    fn img(repository: &str, tag: &str) -> ImageSlot {
        ImageSlot {
            component_path: Vec::new(),
            registry: None,
            repository: Some(repository.to_string()),
            tag: Some(tag.to_string()),
        }
    }

    fn manifest_with(images: Vec<(&str, ImageSlot)>) -> ChartManifest {
        let mut map: BTreeMap<String, ImageSlot> = BTreeMap::new();
        for (slot, image) in images {
            map.insert(slot.to_string(), image);
        }
        ChartManifest {
            schema_version: "1".to_string(),
            name: "test-chart".to_string(),
            chart: "test-chart".to_string(),
            version: "0.0.0".to_string(),
            release: Default::default(),
            bundle: Default::default(),
            deployment: Default::default(),
            lifecycle: Default::default(),
            images: map,
            provenance: Default::default(),
            backup: None,
        }
    }

    #[test]
    fn bitnami_root_repo_is_skipped() {
        assert!(is_bitnami("bitnami/postgresql"));
        assert!(is_bitnami("docker.io/bitnami/redis"));
        assert!(is_bitnami("registry.example.com/bitnami/kafka"));
    }

    #[test]
    fn firestream_repo_is_not_bitnami() {
        assert!(!is_bitnami("firestream-airflow"));
        assert!(!is_bitnami("myorg/firestream-airflow"));
    }

    #[test]
    fn flake_attr_stem_strips_namespace() {
        // Strip a registry namespace; if no `firestream-` prefix is left,
        // pass through.
        assert_eq!(flake_attr_stem("foo/some-repo"), "some-repo");
        assert_eq!(flake_attr_stem(""), "");
    }

    #[test]
    fn flake_attr_stem_strips_firestream_prefix() {
        assert_eq!(flake_attr_stem("firestream-airflow"), "airflow");
        assert_eq!(flake_attr_stem("firestream-os-shell"), "os-shell");
        assert_eq!(flake_attr_stem("firestream-jupyterhub"), "jupyterhub");
        assert_eq!(flake_attr_stem("firestream-postgresql"), "postgresql");
        // Strip namespace AND prefix:
        assert_eq!(flake_attr_stem("myorg/firestream-airflow"), "airflow");
    }

    #[test]
    fn derive_preload_targets_single_slot() {
        let m = manifest_with(vec![("airflow", img("firestream-airflow", "3.0.3"))]);
        let targets = derive_preload_targets(&m);
        assert_eq!(targets.len(), 1);
        let only = targets.iter().next().unwrap();
        assert_eq!(only.flake_attr, ".#airflow-image");
        assert_eq!(only.repo, "firestream-airflow");
        assert_eq!(only.tag, "3.0.3");
    }

    #[test]
    fn derive_preload_targets_jupyterhub_multi_slot_dedup() {
        // jupyterhub has hub/proxy/singleuser all pointing at the same
        // image, plus auxiliaryImage at os-shell, plus a postgres subchart
        // slot. Expect 3 distinct flake apps with no duplicate work.
        let m = manifest_with(vec![
            ("hub", img("firestream-jupyterhub", "5.3.0")),
            ("proxy", img("firestream-jupyterhub", "5.3.0")),
            ("singleuser", img("firestream-jupyterhub", "5.3.0")),
            ("auxiliaryImage", img("firestream-os-shell", "1")),
            ("postgresql", img("firestream-postgresql", "17")),
        ]);
        let targets = derive_preload_targets(&m);
        let attrs: HashSet<String> = targets.iter().map(|t| t.flake_attr.clone()).collect();
        assert_eq!(
            attrs,
            HashSet::from([
                ".#jupyterhub-image".to_string(),
                ".#os-shell-image".to_string(),
                ".#postgresql-image".to_string(),
            ]),
            "expected exactly 3 distinct flake apps; got: {:?}",
            attrs
        );
        assert_eq!(
            targets.len(),
            3,
            "expected dedup to 3 targets; got {}",
            targets.len()
        );
    }

    #[test]
    fn derive_preload_targets_skips_slot_without_repo() {
        let m = manifest_with(vec![
            ("good", img("firestream-airflow", "3.0.3")),
            (
                "no-repo",
                ImageSlot {
                    component_path: Vec::new(),
                    registry: None,
                    repository: None,
                    tag: Some("ignored".to_string()),
                },
            ),
        ]);
        let targets = derive_preload_targets(&m);
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn derive_preload_targets_skips_slot_without_tag() {
        let m = manifest_with(vec![
            ("good", img("firestream-airflow", "3.0.3")),
            (
                "no-tag",
                ImageSlot {
                    component_path: Vec::new(),
                    registry: None,
                    repository: Some("firestream-mystery".to_string()),
                    tag: None,
                },
            ),
        ]);
        let targets = derive_preload_targets(&m);
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn derive_preload_targets_skips_bitnami() {
        let m = manifest_with(vec![
            ("good", img("firestream-airflow", "3.0.3")),
            ("legacy", img("docker.io/bitnami/postgresql", "17.5.0")),
        ]);
        let targets = derive_preload_targets(&m);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets.iter().next().unwrap().repo, "firestream-airflow");
    }

    #[test]
    fn host_import_command_default_is_ctr() {
        // SAFETY: single-threaded test; no other test mutates this var.
        unsafe {
            std::env::remove_var("FIRESTREAM_K8S_IMPORT_CMD");
        }
        let (program, args) = host_import_command();
        assert_eq!(program, "ctr");
        assert_eq!(
            args,
            vec![
                "-a",
                DEFAULT_K3S_CONTAINERD_SOCK,
                "-n",
                "k8s.io",
                "images",
                "import"
            ]
        );
    }

    #[test]
    fn host_import_command_honors_override() {
        // SAFETY: single-threaded test; restored immediately after read.
        unsafe {
            std::env::set_var("FIRESTREAM_K8S_IMPORT_CMD", "sudo ctr -n k8s.io images import");
        }
        let (program, args) = host_import_command();
        unsafe {
            std::env::remove_var("FIRESTREAM_K8S_IMPORT_CMD");
        }
        assert_eq!(program, "sudo");
        assert_eq!(args, vec!["ctr", "-n", "k8s.io", "images", "import"]);
    }
}
