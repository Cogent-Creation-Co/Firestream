//! Crate-wide error enum. Each module owns its own `thiserror` enum;
//! `#[from]` variants here wire those into the top-level `firestream_ci::Error`.
//!
//! Phase 4 wired: util, passthrough, version, rundir, trace, exec,
//! worktree. Phase 5 added: manifest, limits, service, runner, reenter,
//! devshell, oci. Phase 6+ will extend with pipeline / nix / checkpoint /
//! report.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Util(#[from] crate::util::Error),

    #[error(transparent)]
    Passthrough(#[from] crate::passthrough::Error),

    #[error(transparent)]
    Version(#[from] crate::version::Error),

    #[error(transparent)]
    RunDir(#[from] crate::rundir::Error),

    #[error(transparent)]
    Trace(#[from] crate::trace::Error),

    #[error(transparent)]
    Exec(#[from] crate::exec::Error),

    #[error(transparent)]
    Worktree(#[from] crate::worktree::Error),

    #[error(transparent)]
    Manifest(#[from] crate::manifest::Error),

    #[error(transparent)]
    Limits(#[from] crate::limits::Error),

    #[error(transparent)]
    Service(#[from] crate::service::Error),

    #[error(transparent)]
    Runner(#[from] crate::runner::Error),

    #[error(transparent)]
    Reenter(#[from] crate::reenter::Error),

    #[error(transparent)]
    DevShell(#[from] crate::devshell::Error),

    #[error(transparent)]
    Oci(#[from] crate::oci::Error),

    /// Container-image / fleet-manifest build path (the typed equivalent of
    /// `bin/build/container-images.sh` + `bin/build/manifest.sh`).
    #[error(transparent)]
    ImageBuild(#[from] crate::imagebuild::BuildError),

    #[error(transparent)]
    OciFlatten(#[from] crate::oci::FlattenError),

    #[error(transparent)]
    Pipeline(#[from] crate::pipeline::Error),

    #[error(transparent)]
    Nix(#[from] crate::nix::Error),

    #[error(transparent)]
    Checkpoint(#[from] crate::checkpoint::Error),

    #[error(transparent)]
    Report(#[from] crate::report::Error),

    /// CI profile (`ci-manifest.json`) resolution / parse / validation.
    #[error(transparent)]
    Profile(#[from] crate::profile::ProfileError),

    /// Open variant for cases that don't fit a typed module (e.g. config
    /// validation that runs before any module touches its data). Phase
    /// 6+ may add more `#[from]` variants and shrink this.
    #[error("{0}")]
    Other(String),
}
