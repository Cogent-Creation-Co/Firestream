//! End-to-end test harness for the Firestream chart bundle on k3d.
//!
//! Phase 2 of the k3d/helm e2e plan lands this crate with everything
//! needed to spin up and tear down a per-test k3d cluster. Subsequent
//! phases fill in chart deploy, image preload, port-forward, and the
//! per-chart probe matrix.
//!
//! # Layout
//!
//! The reusable k8s/k3d glue — cluster lifecycle, `KubectlExec`,
//! port-forward, readiness wait, and the per-chart probe matrix — was
//! relocated to [`firestream_e2e_core::k8s`] so the main `firestream`
//! CLI can reuse it without forming a dependency cycle. This crate now
//! holds only the firestream-dependent harness layer:
//!
//! - [`env`] — `FIRESTREAM_E2E_K8S_*` accessors. Distinct from core's
//!   `FIRESTREAM_E2E_*` because the docker and k8s harnesses can be
//!   filtered/configured independently.
//! - [`deploy`] — deploys a single chart from the bundle via
//!   `firestream::deploy::helm::deploy_chart_lifecycle` (the reason this
//!   crate depends on `firestream`, and why the glue above had to move
//!   to core rather than the other way around).
//! - [`harness`] — `should_skip_k8s()` + `run_one(chart)`, the full
//!   pipeline wiring the relocated core glue to deploy/images.
//!
//! Image preload was relocated to
//! [`firestream_e2e_core::k8s::images`] so the main `firestream` CLI can
//! reuse it; [`deploy`] calls it via that path (gated by
//! `FIRESTREAM_E2E_K8S_PRELOAD`).

pub mod deploy;
pub mod diagnostics;
pub mod env;
pub mod harness;
pub mod odoo_backup;
pub mod pg_backup;
