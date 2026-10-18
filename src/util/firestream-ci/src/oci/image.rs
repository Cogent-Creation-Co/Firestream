//! Pattern #6 — multi-tier image resolution. Mirrors
//! `bin/_lib.sh::resolve_builder_image` (lines 232-290): walk the fallback
//! chain in order, accept the first hit, fall through to the next tier on
//! miss. Bollard handles local-state inspection + registry pulls; the
//! cold-start tier delegates to a caller-supplied closure (typically
//! `docker build -f <dockerfile> .`) because bollard's build API is its
//! own surface area.
//!
//! Tier classification matches the bash:
//!   * `Local(tag)` — present in the local daemon (`docker image inspect`).
//!     Subject to a size-floor check to skip pathologically small leftover
//!     tags from a prior corrupt commit. The floor is configurable.
//!   * `RegistryBranch(tag)` — branch-suffixed tag in the configured
//!     registry. Skipped on the `main` branch (the next tier covers it).
//!   * `RegistryMain(tag)` — main-branch tag in the registry. The "always
//!     try this" fallback once branch-specific is exhausted.
//!   * `ColdStart(dockerfile, context)` — build from scratch via the
//!     supplied Dockerfile.

use std::path::PathBuf;

use super::{DockerClient, Error, RetryPolicy, retry::pull_with_retry};

/// One tier of the fallback chain. The chain is evaluated head-first.
#[derive(Debug, Clone)]
pub enum Tier {
    /// A locally-present image tag (e.g. `firestream-builder:main-x86_64`).
    Local(String),
    /// A registry-hosted branch tag (e.g.
    /// `gcr.io/.../firestream-builder:<branch>-<arch>`).
    Registry(String),
    /// Cold-start build from a Dockerfile + build context (directory).
    /// `tag` is what we should tag the result as.
    ColdStart {
        dockerfile: PathBuf,
        context: PathBuf,
        tag: String,
    },
}

/// Resolved image — the tag that's now usable + which tier produced it.
#[derive(Debug, Clone)]
pub struct ResolvedImage {
    pub tag: String,
    pub tier_index: usize,
    pub tier_label: &'static str,
}

pub struct Image;

impl Image {
    /// Walk `tiers` in order, returning the first that succeeds.
    /// `min_size_bytes` enforces the bash's size-floor check on Local
    /// hits (pass `0` to disable).
    pub async fn resolve(
        client: &DockerClient,
        tiers: &[Tier],
        platform: &str,
        min_size_bytes: u64,
    ) -> Result<ResolvedImage, Error> {
        let policy = RetryPolicy::default();
        for (idx, tier) in tiers.iter().enumerate() {
            match tier {
                Tier::Local(tag) => {
                    if let Some(size) = inspect_size(client, tag).await? {
                        if min_size_bytes > 0 && size < min_size_bytes {
                            tracing::warn!(
                                target: "firestream_ci::oci",
                                tag, size, floor = min_size_bytes,
                                "local tier hit below size floor; skipping"
                            );
                            continue;
                        }
                        return Ok(ResolvedImage {
                            tag: tag.clone(),
                            tier_index: idx,
                            tier_label: "local",
                        });
                    }
                }
                Tier::Registry(tag) => {
                    if pull_with_retry(client, tag, platform, policy).await.is_ok() {
                        return Ok(ResolvedImage {
                            tag: tag.clone(),
                            tier_index: idx,
                            tier_label: "registry",
                        });
                    }
                }
                Tier::ColdStart {
                    dockerfile,
                    context,
                    tag,
                } => {
                    // Bollard's build API is async + streaming; the
                    // tarball-source pattern requires more wiring than the
                    // bash's `docker build -t … -f … <ctx>`. Shell out for
                    // parity with the bash. Documented per the plan: only
                    // shell out for ops bollard doesn't cover well, with
                    // a comment.
                    build_image_cli(dockerfile, context, tag).await?;
                    return Ok(ResolvedImage {
                        tag: tag.clone(),
                        tier_index: idx,
                        tier_label: "cold-start",
                    });
                }
            }
        }
        Err(Error::NotFound(
            tiers
                .first()
                .map(|t| match t {
                    Tier::Local(t) | Tier::Registry(t) => t.clone(),
                    Tier::ColdStart { tag, .. } => tag.clone(),
                })
                .unwrap_or_else(|| "<no tiers>".to_string()),
        ))
    }

    /// Pull an image (no fallback). Convenience wrapper that exposes the
    /// retry policy. Mirrors the bash `docker_pull_retry`.
    pub async fn pull_with_retry(
        client: &DockerClient,
        tag: &str,
        platform: &str,
        policy: RetryPolicy,
    ) -> Result<(), Error> {
        pull_with_retry(client, tag, platform, policy).await
    }
}

/// Inspect a local image tag. Returns `None` on not-found, `Some(size)` on
/// hit. The bash equivalent is `docker_image_size_bytes` (lines 304-310):
/// returns 0 on missing-image, integer bytes otherwise.
pub async fn inspect_size(client: &DockerClient, tag: &str) -> Result<Option<u64>, Error> {
    use bollard::errors::Error as BErr;
    match client.inner().inspect_image(tag).await {
        Ok(image) => Ok(Some(image.size.unwrap_or(0).max(0) as u64)),
        Err(BErr::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(e) => Err(Error::Bollard(e)),
    }
}

async fn build_image_cli(
    dockerfile: &std::path::Path,
    context: &std::path::Path,
    tag: &str,
) -> Result<(), Error> {
    // Bollard's image build wants a tarballed context — duplicating the
    // CLI's tar machinery here is wasted code for a feature we use once
    // per cold start. Shell out to `docker build`.
    let output = tokio::process::Command::new("docker")
        .arg("build")
        .arg("-t")
        .arg(tag)
        .arg("-f")
        .arg(dockerfile)
        .arg(context)
        .output()
        .await
        .map_err(|source| Error::Io {
            path: dockerfile.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::DockerCli {
            op: "build",
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_constructors() {
        let _ = Tier::Local("img:tag".into());
        let _ = Tier::Registry("reg/img:tag".into());
        let _ = Tier::ColdStart {
            dockerfile: PathBuf::from("Dockerfile"),
            context: PathBuf::from("."),
            tag: "img:tag".into(),
        };
    }

    #[tokio::test]
    async fn resolve_empty_tiers_errors_notfound() {
        let client = match super::super::shared_docker_client() {
            Ok(c) => c,
            Err(_) => return, // no daemon; skip
        };
        let result = Image::resolve(&client, &[], "linux/amd64", 0).await;
        assert!(matches!(result, Err(Error::NotFound(_))));
    }
}
