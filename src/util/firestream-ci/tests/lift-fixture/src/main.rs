//! Synthetic external consumer of `firestream-ci`, compiled with the crate's
//! ordinary default features.
//!
//! The gate: `firestream_ci`'s public API must carry no project-specific
//! coupling at all — no Firestream attr lists, no builder image names, no
//! cargo feature that has to be switched off. Everything project-shaped is
//! runtime data loaded from `ci-manifest.json` (Phase 4). If any module in
//! `firestream_ci::*` grows a compiled-in project assumption that an external
//! consumer cannot satisfy, this fixture stops compiling.

use firestream_ci::{Config, Error, Phase, Pipeline, Tier, Tracer, Verdict};

#[allow(dead_code)]
fn touch_config() -> Result<Config, Error> {
    Config::builder()
        .repo_root("/tmp/repo")
        .build_output_dir("/tmp/_build")
        .build()
}

#[allow(dead_code)]
fn touch_pipeline_types() {
    let _: Option<Pipeline> = None;
    let _: Option<Phase> = None;
    let _: Option<Verdict> = None;
    let _ = Tier::Required;
    let _ = Tier::Advisory;
}

#[allow(dead_code)]
fn touch_tracer() {
    // Don't call .builder() — that's `unimplemented!()` in the skeleton; we
    // only need to prove the type is reachable.
    let _: Option<Tracer> = None;
}

fn main() {
    println!("firestream-ci lift fixture compiled successfully (no project-specific feature exists)");
}
