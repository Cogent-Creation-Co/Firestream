//! Pattern #8 — **the full safety pipeline** for warm-cache lineage.
//! Mirrors `bin/_lib.sh::commit_flatten_builder` + `_commit_flatten_reap`
//! (lines 286-510). Read both functions end-to-end before modifying this
//! file; the bash version was incrementally debugged in production and
//! every guard reflects a real failure mode.
//!
//! ## The pipeline (one diagram per the plan)
//!
//! ```text
//!     ┌─ Pre-flatten snapshot (prev_canonical_id, pre_dangling_ids)
//!     │
//!     ▼
//! 1.  Apply pending tag (<tag>-pending-<pid>)
//!     │
//!     ▼
//! 2.  Capture src_image runtime config (ENV, WORKDIR, USER, ENTRYPOINT)
//!     │
//!     ▼
//! 3.  docker export <container> | docker import --change ... - <pending>
//!     │
//!     ▼
//! 4.  Size validation: inspect <pending>.size; reject if < MIN_BYTES
//!     │
//!     ▼
//! 5.  Atomic retag: docker tag <pending> <canonical>; rmi <pending>
//!     │
//!     ▼
//! 6.  Apply lineage labels (firestream.lineage.*)
//!     │
//!     ▼
//! 7.  Scoped reaper (Step A stopped containers, Step B same-arch tags,
//!                    Step C newly-dangling images), preserving other-arch
//!                    canonicals.
//! ```
//!
//! ## Lineage labels (full list)
//!
//! Per the plan: `firestream.lineage.branch`, `firestream.lineage.sha`, `firestream.lineage.epoch`,
//! `firestream.lineage.parent_sha`, `firestream.lineage.trace_id`. We also emit
//! `firestream.lineage.arch` and `firestream.lineage.flatten_ts` because the bash captures
//! the same info inside `_phase3_export_target`'s manifest entry, and
//! downstream consumers ask for them. Labels are written via a docker CLI
//! shell-out (`docker label`-equivalent: `docker image inspect` shows them
//! after a `--change LABEL=...` import).
//!
//! ## Size floor
//!
//! `MIN_BUILDER_IMAGE_SIZE_BYTES = 100 MiB` (104857600). Matches the bash
//! constant. Overridable via env var to keep parity with the bash override
//! pathway.

use std::collections::{BTreeSet, HashMap};

use bollard::container::DownloadFromContainerOptions;
use bollard::image::{ListImagesOptions, RemoveImageOptions, TagImageOptions};
use futures::TryStreamExt;
use thiserror::Error as ThisError;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use tracing::{info, warn};

use super::{DockerClient, Error};

/// 100 MiB — the schema default for `builder.min_image_size_bytes`.
///
/// The authoritative value is profile data
/// ([`crate::profile::Builder::min_image_size_bytes`]); every caller passes it
/// explicitly through `commit_flatten_builder`'s `min_size_bytes` argument.
/// This constant survives only as the value the profile schema falls back to
/// when the key is absent, and as the `--min-size` flag's documented default.
///
/// Lineage: ConceptDB's `bin/_lib.sh::MIN_BUILDER_IMAGE_SIZE_BYTES`, preserved
/// in `docs/defaults-reference.rs.txt`.
pub const MIN_BUILDER_IMAGE_SIZE_BYTES: u64 = 104_857_600;

#[derive(Debug, ThisError)]
pub enum FlattenError {
    #[error(transparent)]
    Oci(#[from] Error),
    #[error("flatten: size validation failed: {size} bytes < floor {floor}")]
    SizeFloor { size: u64, floor: u64 },
    #[error("flatten: docker export/import shell-out failed ({code}): {stderr}")]
    ExportImport { code: i32, stderr: String },
    #[error("flatten: malformed tag `{0}` (expected name:branch-arch)")]
    BadTag(String),
}

/// All lineage labels we set on the flattened image. The full set is
/// captured here so Phase 8 parity verification (and tests) can assert
/// against an explicit constant.
#[derive(Debug, Clone, Default)]
pub struct LineageLabels {
    pub branch: String,
    pub sha: String,
    pub epoch: String,
    pub parent_sha: String,
    pub trace_id: String,
    pub arch: String,
    pub flatten_ts: String,
}

impl LineageLabels {
    /// Render as `--change LABEL k=v` argument fragments suitable for
    /// `docker import` invocation. Empty values produce empty labels (the
    /// bash equivalent never quotes empty strings; we follow suit).
    pub fn to_docker_changes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let push = |out: &mut Vec<String>, k: &str, v: &str| {
            out.push("--change".to_string());
            out.push(format!("LABEL {k}={v}"));
        };
        push(&mut out, "firestream.lineage.branch", &self.branch);
        push(&mut out, "firestream.lineage.sha", &self.sha);
        push(&mut out, "firestream.lineage.epoch", &self.epoch);
        push(&mut out, "firestream.lineage.parent_sha", &self.parent_sha);
        push(&mut out, "firestream.lineage.trace_id", &self.trace_id);
        push(&mut out, "firestream.lineage.arch", &self.arch);
        push(&mut out, "firestream.lineage.flatten_ts", &self.flatten_ts);
        out
    }

    /// All label keys we emit. Phase 8 parity check uses this list to
    /// verify the bash + Rust label sets match.
    pub const KEYS: &'static [&'static str] = &[
        "firestream.lineage.branch",
        "firestream.lineage.sha",
        "firestream.lineage.epoch",
        "firestream.lineage.parent_sha",
        "firestream.lineage.trace_id",
        "firestream.lineage.arch",
        "firestream.lineage.flatten_ts",
    ];
}

/// Per-image classification result from the reaper. Exposed so callers
/// (and Phase 8 tests) can assert on the classification BEFORE any
/// removals happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapClassification {
    /// Same-arch non-canonical tag. Reap.
    SameArchNonCanonical,
    /// Legacy tag without an arch suffix. Reap.
    Legacy,
    /// Other-arch canonical tag. Preserve.
    OtherArchCanonical,
    /// Current canonical (the tag we just retagged to). Preserve.
    NewCanonical,
    /// Dangling image that became dangling during this run. Reap.
    NewlyDangling,
    /// Dangling image that was already dangling BEFORE this run. Preserve.
    PreExistingDangling,
    /// The previous canonical (orphaned by the retag). Reap.
    PreviousCanonical,
}

impl ReapClassification {
    pub fn should_reap(&self) -> bool {
        matches!(
            self,
            Self::SameArchNonCanonical
                | Self::Legacy
                | Self::NewlyDangling
                | Self::PreviousCanonical
        )
    }
}

#[derive(Debug, Clone)]
pub struct FlattenResult {
    pub tag: String,
    pub size_bytes: u64,
    pub pending_tag: String,
    pub reaped_count: usize,
    pub labels: LineageLabels,
}

/// Persist a finished CI container as a single-layer warm-cache image,
/// repointing `tag`. Mirrors `commit_flatten_builder` end-to-end.
///
/// Args:
///   * `container_name` — stopped is fine
///   * `tag` — target canonical (e.g. `firestream-builder:main-x86_64`)
///   * `src_image` — the image the container was created from (used to
///     re-apply ENV/WORKDIR/USER on import)
///   * `labels` — lineage labels to embed
///   * `image_name_for_reaper` — bare image name (e.g. `firestream-builder`)
///     used to scope the reaper's `reference=` filter
///   * `current_arch` — e.g. "x86_64"; reaper preserves other-arch canonicals
///   * `known_arches` — full set the reaper recognizes
///   * `container_name_prefix` — name filter the reaper uses to find stopped
///     CI containers (profile data: `builder.container_name_prefix`, default
///     `"<project>-ci-"`). Empty string disables that reaper step.
///
/// Returns the flatten result on success. On failure, the pending tag is
/// cleaned up and the prior canonical is left untouched.
#[allow(clippy::too_many_arguments)]
pub async fn commit_flatten_builder(
    client: &DockerClient,
    container_name: &str,
    tag: &str,
    src_image: Option<&str>,
    labels: LineageLabels,
    image_name_for_reaper: &str,
    current_arch: &str,
    known_arches: &[&str],
    min_size_bytes: u64,
    container_name_prefix: &str,
) -> Result<FlattenResult, FlattenError> {
    // Step 0: pending tag.
    let pid = std::process::id();
    let pending_tag = format!("{tag}-pending-{pid}");

    // Step 1: pre-flatten snapshots for the reaper.
    let prev_canonical_id = inspect_image_id(client, tag).await;
    let pre_dangling_ids = collect_dangling_ids(client).await;

    // Step 2: capture src_image runtime config + compose --change list.
    let mut changes: Vec<String> = Vec::new();
    if let Some(src) = src_image {
        let src_changes = capture_src_image_changes(client, src).await;
        changes.extend(src_changes);
    }
    // Re-bake the dev-env entrypoint so a plain `docker run` of the warm
    // image still boots the shell. Matches bash line 384.
    changes.push("--change".to_string());
    changes.push("ENTRYPOINT [\"/nix-keep/entrypoint.sh\"]".to_string());
    // Lineage labels.
    changes.extend(labels.to_docker_changes());

    // Step 3: docker export <container> | docker import --change... - <pending>.
    // Bollard does not cover `docker import`, so we shell out for this step.
    // The export tarball is streamed through the shell pipe — keeping it
    // off-disk avoids the host-side disk pressure the bash warns about.
    let er = run_export_import_pipeline(container_name, &changes, &pending_tag).await?;
    if !er.success {
        // Clean up the pending tag if it landed.
        let _ = remove_image_force(client, &pending_tag).await;
        return Err(FlattenError::ExportImport {
            code: er.code,
            stderr: er.stderr,
        });
    }

    // Step 4: size validation.
    let size_bytes = inspect_image_size(client, &pending_tag).await.unwrap_or(0);
    if size_bytes < min_size_bytes {
        warn!(
            target: "firestream_ci::oci::flatten",
            pending_tag, size = size_bytes, floor = min_size_bytes,
            "pending image below size floor; rejecting"
        );
        let _ = remove_image_force(client, &pending_tag).await;
        return Err(FlattenError::SizeFloor {
            size: size_bytes,
            floor: min_size_bytes,
        });
    }

    // Step 5: atomic retag via bollard.
    let (repo, tag_part) = split_tag(tag).ok_or_else(|| FlattenError::BadTag(tag.into()))?;
    client
        .inner()
        .tag_image(
            &pending_tag,
            Some(TagImageOptions {
                repo,
                tag: tag_part,
            }),
        )
        .await
        .map_err(|e| FlattenError::Oci(Error::Bollard(e)))?;
    // Drop the pending tag — best-effort.
    let _ = remove_image_force(client, &pending_tag).await;

    // Step 6: reaper. Failures are non-fatal: the flatten + retag succeeded.
    let new_canonical_id = inspect_image_id(client, tag).await;
    let reaped_count = match new_canonical_id.as_ref() {
        Some(nc) => run_reaper(
            client,
            tag,
            nc,
            prev_canonical_id.as_deref(),
            &pre_dangling_ids,
            image_name_for_reaper,
            current_arch,
            known_arches,
            container_name_prefix,
        )
        .await
        .unwrap_or(0),
        None => {
            warn!(target: "firestream_ci::oci::flatten", "post-tag lookup failed; skipping reap");
            0
        }
    };

    info!(
        target: "firestream_ci::oci::flatten",
        tag, size_bytes, reaped_count,
        "warm-cache flatten complete"
    );

    Ok(FlattenResult {
        tag: tag.to_string(),
        size_bytes,
        pending_tag,
        reaped_count,
        labels,
    })
}

/// Classify one same-arch / legacy tag candidate. Pure — no docker calls.
/// Exposed for unit tests + Phase 8 parity verification.
pub fn classify_tag(
    tag_part: &str,
    image_id: &str,
    new_canonical_id: &str,
    current_arch: &str,
    known_arches: &[&str],
) -> ReapClassification {
    if image_id == new_canonical_id {
        return ReapClassification::NewCanonical;
    }
    if tag_part == "<none>" {
        // Caller deals with dangling separately; this path is name-tagged.
        return ReapClassification::Legacy;
    }
    // Other-arch canonical preservation: any tag suffixed with a known
    // arch OTHER than current_arch.
    for arch in known_arches {
        if *arch == current_arch {
            continue;
        }
        let suffix = format!("-{arch}");
        if tag_part.ends_with(&suffix) {
            return ReapClassification::OtherArchCanonical;
        }
    }
    // If it ends with the current arch, it's same-arch non-canonical.
    let cur_suffix = format!("-{current_arch}");
    if tag_part.ends_with(&cur_suffix) {
        return ReapClassification::SameArchNonCanonical;
    }
    // Otherwise it's a legacy non-arch-suffixed tag.
    ReapClassification::Legacy
}

/// Classify a dangling image. Pre-existing (in `pre_dangling_ids`) ⇒
/// preserve. Otherwise ⇒ reap. The bash uses `comm -23 post pre`.
pub fn classify_dangling(
    image_id: &str,
    pre_dangling_ids: &BTreeSet<String>,
) -> ReapClassification {
    if pre_dangling_ids.contains(image_id) {
        ReapClassification::PreExistingDangling
    } else {
        ReapClassification::NewlyDangling
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_reaper(
    client: &DockerClient,
    _tag: &str,
    new_canonical_id: &str,
    prev_canonical_id: Option<&str>,
    pre_dangling_ids: &BTreeSet<String>,
    image_name_for_reaper: &str,
    current_arch: &str,
    known_arches: &[&str],
    container_name_prefix: &str,
) -> Result<usize, Error> {
    let mut reap_ids: BTreeSet<String> = BTreeSet::new();

    // Step A: stopped CI containers. The name filter is profile data
    // (`builder.container_name_prefix`, defaulting to `"<project>-ci-"`), so
    // this step knows nothing about which project it is reaping for. An empty
    // prefix would match every stopped container on the host — refuse it.
    if container_name_prefix.is_empty() {
        warn!(
            target: "firestream_ci::oci::flatten",
            "empty container_name_prefix; skipping the stopped-container reap step"
        );
    } else {
        reap_stopped_ci_containers(client, container_name_prefix).await;
    }

    // Step B: same-arch / legacy builder tags.
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert(
        "reference".to_string(),
        vec![image_name_for_reaper.to_string()],
    );
    let images = client
        .inner()
        .list_images(Some(ListImagesOptions::<String> {
            all: false,
            filters,
            ..Default::default()
        }))
        .await?;
    for img in &images {
        for tag_full in &img.repo_tags {
            // RepoTags are `<repo>:<tag>` strings.
            let (_repo, tag_part) = match tag_full.rsplit_once(':') {
                Some(p) => p,
                None => continue,
            };
            let kind = classify_tag(
                tag_part,
                &img.id,
                new_canonical_id,
                current_arch,
                known_arches,
            );
            if kind.should_reap() {
                reap_ids.insert(img.id.clone());
            }
        }
    }

    // Step C: newly-dangling images.
    let post_dangling = collect_dangling_ids(client).await;
    for id in &post_dangling {
        let kind = classify_dangling(id, pre_dangling_ids);
        if kind.should_reap() && id != new_canonical_id {
            reap_ids.insert(id.clone());
        }
    }

    // Step D: explicitly include the previous canonical (orphaned by retag).
    if let Some(prev) = prev_canonical_id {
        if prev != new_canonical_id {
            reap_ids.insert(prev.to_string());
        }
    }

    let mut reaped = 0usize;
    for id in &reap_ids {
        if remove_image_force(client, id).await.is_ok() {
            reaped += 1;
        }
    }
    Ok(reaped)
}

async fn capture_src_image_changes(client: &DockerClient, src_image: &str) -> Vec<String> {
    let mut out = Vec::new();
    let inspect = match client.inner().inspect_image(src_image).await {
        Ok(x) => x,
        Err(_) => return out,
    };
    if let Some(cfg) = inspect.config {
        if let Some(envs) = cfg.env {
            for e in envs {
                out.push("--change".to_string());
                out.push(format!("ENV {e}"));
            }
        }
        if let Some(wd) = cfg.working_dir {
            if !wd.is_empty() {
                out.push("--change".to_string());
                out.push(format!("WORKDIR {wd}"));
            }
        }
        if let Some(user) = cfg.user {
            if !user.is_empty() {
                out.push("--change".to_string());
                out.push(format!("USER {user}"));
            }
        }
    }
    out
}

struct ExportImportResult {
    success: bool,
    code: i32,
    stderr: String,
}

/// `docker export <container> | docker import --change ... - <pending_tag>`.
/// We use the actual shell pipe (a single bash invocation) so the stream
/// stays off-disk and matches the bash behavior byte-for-byte.
async fn run_export_import_pipeline(
    container_name: &str,
    changes: &[String],
    pending_tag: &str,
) -> Result<ExportImportResult, FlattenError> {
    // Compose: docker export <C> | docker import [--change …] - <pending>
    let mut script = String::new();
    script.push_str("set -o pipefail; docker export ");
    script.push_str(&shell_escape(container_name));
    script.push_str(" | docker import");
    let mut i = 0;
    while i < changes.len() {
        // --change KEY VAL pairs come in twos.
        script.push(' ');
        script.push_str(&shell_escape(&changes[i]));
        script.push(' ');
        script.push_str(&shell_escape(&changes[i + 1]));
        i += 2;
    }
    script.push_str(" - ");
    script.push_str(&shell_escape(pending_tag));

    let output = TokioCommand::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .map_err(|source| {
            FlattenError::Oci(Error::Io {
                path: std::path::PathBuf::from("bash"),
                source,
            })
        })?;
    Ok(ExportImportResult {
        success: output.status.success(),
        code: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Bollard-based export to a sink — alternative to the CLI pipeline above.
/// Not currently used by `commit_flatten_builder` (the CLI pipe keeps
/// stream semantics parity with the bash), but exposed for callers who
/// only need the export half (e.g. tests).
pub async fn export_container_to_writer<W: AsyncWriteExt + Unpin>(
    client: &DockerClient,
    container_name: &str,
    sink: &mut W,
) -> Result<u64, Error> {
    let mut stream = client.inner().download_from_container(
        container_name,
        Some(DownloadFromContainerOptions { path: "/" }),
    );
    let mut total: u64 = 0;
    while let Some(chunk) = stream.try_next().await? {
        let bytes = chunk;
        sink.write_all(&bytes).await.map_err(|source| Error::Io {
            path: std::path::PathBuf::from("<sink>"),
            source,
        })?;
        total += bytes.len() as u64;
    }
    Ok(total)
}

async fn inspect_image_id(client: &DockerClient, tag: &str) -> Option<String> {
    client
        .inner()
        .inspect_image(tag)
        .await
        .ok()
        .map(|i| i.id.unwrap_or_default())
}

async fn inspect_image_size(client: &DockerClient, tag: &str) -> Option<u64> {
    client
        .inner()
        .inspect_image(tag)
        .await
        .ok()
        .map(|i| i.size.unwrap_or(0).max(0) as u64)
}

async fn collect_dangling_ids(client: &DockerClient) -> BTreeSet<String> {
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("dangling".to_string(), vec!["true".to_string()]);
    let images = client
        .inner()
        .list_images(Some(ListImagesOptions::<String> {
            all: false,
            filters,
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    images.into_iter().map(|i| i.id).collect()
}

async fn remove_image_force(client: &DockerClient, id: &str) -> Result<(), Error> {
    let opts = RemoveImageOptions {
        force: true,
        noprune: false,
    };
    client.inner().remove_image(id, Some(opts), None).await?;
    Ok(())
}

async fn reap_stopped_ci_containers(client: &DockerClient, name_prefix: &str) {
    use bollard::container::ListContainersOptions;
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("status".to_string(), vec!["exited".to_string()]);
    filters.insert("name".to_string(), vec![name_prefix.to_string()]);
    let containers = match client
        .inner()
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(_) => return,
    };
    for c in containers {
        if let Some(id) = c.id {
            let _ = client
                .inner()
                .remove_container(&id, None::<bollard::container::RemoveContainerOptions>)
                .await;
        }
    }
}

fn split_tag(tag: &str) -> Option<(String, String)> {
    let (repo, t) = tag.rsplit_once(':')?;
    Some((repo.to_string(), t.to_string()))
}

fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_labels_keys_match_constant() {
        let labels = LineageLabels {
            branch: "main".into(),
            sha: "abc12345".into(),
            epoch: "1716831234".into(),
            parent_sha: "old1".into(),
            trace_id: "tid".into(),
            arch: "x86_64".into(),
            flatten_ts: "1716831290".into(),
        };
        let changes = labels.to_docker_changes();
        // 7 labels × 2 args each = 14
        assert_eq!(changes.len(), LineageLabels::KEYS.len() * 2);
        for key in LineageLabels::KEYS {
            assert!(
                changes
                    .iter()
                    .any(|c| c.starts_with(&format!("LABEL {key}="))),
                "missing label {key}"
            );
        }
    }

    #[test]
    fn classify_tag_other_arch_preserved() {
        let kind = classify_tag(
            "main-aarch64",
            "abc",
            "new",
            "x86_64",
            &["x86_64", "aarch64"],
        );
        assert_eq!(kind, ReapClassification::OtherArchCanonical);
        assert!(!kind.should_reap());
    }

    #[test]
    fn classify_tag_same_arch_noncanonical_reaped() {
        let kind = classify_tag(
            "feature-x-x86_64",
            "abc",
            "new",
            "x86_64",
            &["x86_64", "aarch64"],
        );
        assert_eq!(kind, ReapClassification::SameArchNonCanonical);
        assert!(kind.should_reap());
    }

    #[test]
    fn classify_tag_legacy_no_arch_suffix_reaped() {
        let kind = classify_tag("latest", "abc", "new", "x86_64", &["x86_64", "aarch64"]);
        assert_eq!(kind, ReapClassification::Legacy);
        assert!(kind.should_reap());
    }

    #[test]
    fn classify_tag_new_canonical_preserved() {
        let kind = classify_tag(
            "main-x86_64",
            "new",
            "new",
            "x86_64",
            &["x86_64", "aarch64"],
        );
        assert_eq!(kind, ReapClassification::NewCanonical);
        assert!(!kind.should_reap());
    }

    #[test]
    fn classify_dangling_pre_existing_preserved() {
        let mut pre = BTreeSet::new();
        pre.insert("sha256:aaa".to_string());
        let k = classify_dangling("sha256:aaa", &pre);
        assert_eq!(k, ReapClassification::PreExistingDangling);
        assert!(!k.should_reap());
        let k2 = classify_dangling("sha256:bbb", &pre);
        assert_eq!(k2, ReapClassification::NewlyDangling);
        assert!(k2.should_reap());
    }

    #[test]
    fn split_tag_repo_and_tag() {
        assert_eq!(
            split_tag("firestream-builder:main-x86_64"),
            Some(("firestream-builder".to_string(), "main-x86_64".to_string()))
        );
        assert_eq!(split_tag("notag"), None);
    }

    #[test]
    fn shell_escape_single_quotes_safe() {
        assert_eq!(shell_escape("hello"), "'hello'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn min_builder_image_size_matches_bash_constant() {
        // bin/_lib.sh::MIN_BUILDER_IMAGE_SIZE_BYTES = 104857600 (100 MiB)
        assert_eq!(MIN_BUILDER_IMAGE_SIZE_BYTES, 104_857_600);
    }
}
