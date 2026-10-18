//! OCI patterns #6, #7, #8, #9 — image resolution, retry-with-backoff,
//! flatten safety pipeline, source sync. Bollard is the workhorse for
//! local-state inspection and container lifecycle; the cli `docker` is
//! shelled out for the handful of operations bollard doesn't cover
//! (`import`, manifest manipulation, ad-hoc registry login).
//!
//! Each sub-module is documented with its bash source-of-truth line range
//! at the top of the file.

pub mod client;
pub mod flatten;
pub mod image;
pub mod retry;
pub mod source_sync;

pub use client::{DockerClient, shared_docker_client};
pub use flatten::{
    FlattenError, FlattenResult, LineageLabels, ReapClassification, commit_flatten_builder,
};
pub use image::{Image, Tier};
pub use retry::{RetryPolicy, pull_with_retry};
pub use source_sync::{GitLsFiles, copy_source_to_container};

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("oci: bollard error: {0}")]
    Bollard(#[from] bollard::errors::Error),
    #[error("oci: docker CLI shell-out failed ({op}; code={code}): {stderr}")]
    DockerCli {
        op: &'static str,
        code: i32,
        stderr: String,
    },
    #[error("oci: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("oci: image `{0}` not found in any fallback tier")]
    NotFound(String),
    #[error("oci: image `{tag}` failed size validation ({size} bytes < floor {floor})")]
    SizeFloor { tag: String, size: u64, floor: u64 },
    #[error("oci: flatten failure: {0}")]
    Flatten(String),
    #[error("oci: source-sync git ls-files failed ({code}): {stderr}")]
    GitLsFiles { code: i32, stderr: String },
}
