//! Per-derivation `nix build` orchestration.

pub mod log;
pub mod nix_build;

use std::collections::BTreeMap;

/// A derivation that nix-eval-jobs surfaced and that the build queue is
/// supposed to realise (or, in side queues, push to a cache / download from a
/// remote builder).
#[derive(Debug, Clone)]
pub struct Build {
    pub attr: String,
    pub drv_path: String,
    pub outputs: BTreeMap<String, String>,
}

impl Build {
    pub fn from_job(job: crate::eval::Job) -> Self {
        Self {
            attr: job.attr,
            drv_path: job.drv_path,
            outputs: job.outputs,
        }
    }
}

#[derive(Debug)]
pub struct BuildResult {
    pub return_code: i32,
    pub log_output: String,
}
