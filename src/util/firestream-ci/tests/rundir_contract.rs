//! Phase 7 — regression dam for the `_build/` rundir refactor.
//!
//! Drives the public `firestream_ci::*` surface against a tmpdir to assert the
//! whole on-disk layout produced by a finished run:
//!   - `<rundir>/.sentinel` with the rundir path as content,
//!   - `<rundir>/artifacts/<dest>/...` with copied bytes + manifest entry
//!     carrying `sha256`, `size_bytes`, `copy_mode`, `nix_store_path`,
//!   - `<rundir>/manifest.json` with `schema_version`, `run.*` populated
//!     (incl. `outcome`),
//!   - `<rundir>/profiles/spans.ndjson` line count matches spans on disk.
//!
//! The binary's `finalize_manifest` resolves `branch`/`mode` from env and
//! is private to `bin/firestream-ci.rs`; rather than spawn the binary or refactor
//! the function out, this test inlines the three-step finalize (open +
//! with_run_meta + finalize, plus the ndjson aggregation) so the test can be
//! hermetic and async.

use std::path::Path;

use opentelemetry_proto::tonic::trace::v1::{Span as ProtoSpan, Status};

use firestream_ci::artifacts::{ArtifactsFormat, export_build_artifact};
use firestream_ci::manifest::{Manifest, ManifestFile, Outcome, RunMeta};
use firestream_ci::report::Report;
use firestream_ci::rundir::RunDir;

/// Minimal proto span with valid ids/timing; written to
/// `<spans_dir>/<trace_hex>/<span_hex>/span.json` so `Report::from_span_dir`
/// picks it up.
fn write_synthetic_span(spans_dir: &Path) -> (String, String) {
    let span = ProtoSpan {
        trace_id: vec![0xaa; 16],
        span_id: vec![0x11; 8],
        parent_span_id: vec![],
        name: "phase.test".into(),
        attributes: vec![],
        status: Some(Status {
            message: String::new(),
            code: 1,
        }),
        start_time_unix_nano: 1_000_000_000,
        end_time_unix_nano: 3_500_000_000,
        ..Default::default()
    };
    let trace_hex = hex::encode(&span.trace_id);
    let span_hex = hex::encode(&span.span_id);
    let dir = spans_dir.join(&trace_hex).join(&span_hex);
    std::fs::create_dir_all(&dir).expect("mkdir spans subtree");
    let bytes = serde_json::to_vec_pretty(&span).expect("serialize proto span");
    std::fs::write(dir.join("span.json"), bytes).expect("write span.json");
    (trace_hex, span_hex)
}

/// Stub a `nix-fast-build` per-attr result file pointing at a fake Nix store
/// output: a *directory* containing one 5-byte file (the `dockerTools.buildImage`
/// shape, where `$out` is a dir with the payload plus sidecars). Mirrors the
/// JSON shape `artifacts::read_store_path` consumes. 5 source bytes per the
/// Phase 7 spec — keeps the size_bytes assertion exact.
fn stub_nix_result_and_store(tmp: &Path, attr: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let store_dir = tmp.join("nix-store-out-demo-server");
    std::fs::create_dir_all(&store_dir).expect("create fake store dir");
    let store_tar = store_dir.join("image.tar.gz");
    std::fs::write(&store_tar, b"hello").expect("write fake store tarball");
    assert_eq!(std::fs::metadata(&store_tar).unwrap().len(), 5);

    let result_path = tmp.join("nix-fast-build-build.server.json");
    let result_doc = serde_json::json!({
        "results": [
            {
                "attr": attr,
                "success": true,
                "outputs": { "out": store_dir.to_string_lossy() }
            }
        ]
    });
    std::fs::write(&result_path, serde_json::to_vec(&result_doc).unwrap())
        .expect("write nix-fast-build result");
    (result_path, store_dir)
}

#[tokio::test]
async fn successful_run_produces_full_layout() {
    let base = tempfile::tempdir().expect("base tempdir");
    let stub_root = tempfile::tempdir().expect("stub tempdir");

    // 1. Allocate a rundir. The library writes `.sentinel` + subdirs as
    //    part of `create`.
    let rundir = RunDir::create(base.path(), "deadbeef")
        .await
        .expect("RunDir::create");

    // 2. `.sentinel` exists and contains the rundir path string.
    let sentinel_path = rundir.path().join(".sentinel");
    assert!(sentinel_path.exists(), "sentinel must exist after create");
    let sentinel_text = std::fs::read_to_string(&sentinel_path).expect("read sentinel");
    assert_eq!(
        sentinel_text,
        rundir.path().to_string_lossy(),
        "sentinel contents must equal the rundir path"
    );

    // 3. Stub a fake Nix store output + per-attr result JSON, then call
    //    `export_build_artifact`.
    //
    //    Phase 4: `artifacts::resolve_target` is now a lookup into the CI
    //    profile's `export_targets`. This test supplies its own synthetic
    //    profile whose first rule classifies `demo-*-linux-*` as `image` —
    //    the shape ConceptDB's `build_export_target` produced — so the
    //    dockerTools-tarball fast path in `perform_export` runs and the file
    //    lands NESTED at `<artifacts>/<dest>/<filename>`. That nested-path
    //    assertion is what this test exists to pin.
    let profile: firestream_ci::Profile = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "project": { "name": "demo", "nix_system": "x86_64-linux", "arch": "x86_64" },
        "export_targets": [
            { "match": { "prefix": "demo-", "contains": "-linux-" },
              "dest": "{leaf}", "kind": "image" }
        ],
        "export_default": { "dest": "{leaf}", "kind": "binary" }
    }))
    .expect("synthetic profile parses");

    let attr = "packages.x86_64-linux.demo-server-linux-x86_64";
    let (result_file, _store_tar) = stub_nix_result_and_store(stub_root.path(), attr);

    export_build_artifact(
        &profile,
        attr,
        "x86_64",
        &rundir,
        &result_file,
        ArtifactsFormat::Copy,
        10_000_000,
    )
    .await;

    // The attr leaf `demo-server-linux-x86_64` matches the `image` rule, so
    // dest is the leaf and the tarball lands under `<artifacts>/<dest>/`.
    let dest_dir = rundir.artifacts_dir().join("demo-server-linux-x86_64");
    let copied = dest_dir.join("image.tar.gz");
    assert!(
        copied.exists(),
        "exported tarball must exist at {}",
        copied.display()
    );
    let copied_bytes = std::fs::read(&copied).expect("read copied tarball");
    assert_eq!(
        copied_bytes, b"hello",
        "copied bytes must match the source tarball"
    );

    // 5. Drop one synthetic span so the NDJSON aggregation has something to
    //    pick up.
    let (_trace_hex, _span_hex) = write_synthetic_span(rundir.spans_dir());

    // 6. Inline the three-step finalize that `bin/firestream-ci.rs::finalize_manifest`
    //    performs. We pass explicit branch/mode/outcome so the test asserts
    //    against known-good values without depending on env state.
    let meta = RunMeta {
        branch: "main".into(),
        mode: "release".into(),
        host_os: "linux".into(),
        arch: "x86_64".into(),
        outcome: Some(Outcome::Passed),
        ..Default::default()
    };
    let manifest = Manifest::open(&rundir)
        .await
        .expect("Manifest::open")
        .with_run_meta(meta);
    manifest.finalize().await.expect("Manifest::finalize");

    let report = Report::from_span_dir(rundir.spans_dir()).expect("Report::from_span_dir");
    let ndjson_path = rundir.profiles_dir().join("spans.ndjson");
    report
        .write_ndjson(&ndjson_path)
        .expect("Report::write_ndjson");

    // 7. Assertions on the manifest file.
    let manifest_path = rundir.path().join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json must exist");
    let manifest_bytes = std::fs::read(&manifest_path).expect("read manifest");
    let file: ManifestFile = serde_json::from_slice(&manifest_bytes).expect("parse manifest.json");
    assert_eq!(file.schema_version, 2, "schema_version pinned to 2");
    assert_eq!(file.run.branch, "main");
    assert_eq!(file.run.mode, "release");
    assert_eq!(file.run.host_os, "linux");
    assert_eq!(file.run.arch, "x86_64");
    assert_eq!(file.run.outcome, Some(Outcome::Passed));
    assert_eq!(file.run.sha, "deadbeef");
    assert!(file.run.epoch > 0, "epoch inferred from rundir layout");

    // 8. Artifact entry shape: every attr asserted by the Phase 7 spec.
    assert!(
        !file.artifacts.is_empty(),
        "manifest must carry at least one artifact entry"
    );
    let entry = file
        .artifacts
        .iter()
        .find(|e| e.dest_subdir.to_string_lossy() == "demo-server-linux-x86_64")
        .expect("manifest carries the demo-server entry");
    assert!(
        entry.attrs.contains_key("sha256"),
        "entry must record sha256; got attrs={:?}",
        entry.attrs
    );
    assert_eq!(
        entry.attrs.get("size_bytes").map(String::as_str),
        Some("5"),
        "size_bytes records the source byte count"
    );
    assert_eq!(
        entry.attrs.get("copy_mode").map(String::as_str),
        Some("copy"),
        "copy_mode records the materialisation mode"
    );
    assert!(
        entry
            .attrs
            .get("nix_store_path")
            .map(|s| s.contains("nix-store-out-demo-server"))
            .unwrap_or(false),
        "nix_store_path records the source store path"
    );

    // 9. NDJSON sanity.
    assert!(ndjson_path.exists(), "profiles/spans.ndjson must exist");
    let body = std::fs::read_to_string(&ndjson_path).expect("read ndjson");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1, "one synthetic span → one ndjson record");
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("ndjson line parses as JSON");
    let duration_ns = parsed
        .get("duration_ns")
        .and_then(|v| v.as_u64())
        .expect("duration_ns must be u64");
    assert!(duration_ns > 0, "synthetic span has positive duration_ns");
}

#[tokio::test]
async fn aborted_run_finalizer_writes_aborted_outcome() {
    let base = tempfile::tempdir().expect("base tempdir");

    let rundir = RunDir::create(base.path(), "deadbeef")
        .await
        .expect("RunDir::create");

    // Skip the work-doing phases. Drive the abort path directly: set
    // outcome=Aborted and finalize the manifest.
    let meta = RunMeta {
        branch: "main".into(),
        mode: "release".into(),
        host_os: "linux".into(),
        arch: "x86_64".into(),
        outcome: Some(Outcome::Aborted),
        ..Default::default()
    };
    let manifest = Manifest::open(&rundir)
        .await
        .expect("Manifest::open")
        .with_run_meta(meta);
    manifest.finalize().await.expect("Manifest::finalize");

    // Manifest reflects the aborted outcome.
    let manifest_path = rundir.path().join("manifest.json");
    assert!(
        manifest_path.exists(),
        "aborted runs still write manifest.json"
    );
    let file: ManifestFile =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest.json");
    assert_eq!(
        file.run.outcome,
        Some(Outcome::Aborted),
        "outcome must round-trip as aborted"
    );
}

// ---------------------------------------------------------------------------
// Build-root anchoring.
//
// Regression dam for the stray `src/util/_build/<date>/<sha>/<epoch>/.sentinel`
// tree: `allocate_run_dir` used to anchor at `std::env::current_dir()`, so
// `cd src/util && cargo run -p firestream-ci ...` (what `make build-util` and
// `make test-util` do) minted a second `_build/` root — and one sentinel, whose
// contents are an absolute host path, got staged into git. There is exactly one
// `_build/`, and it lives at the repo root.
// ---------------------------------------------------------------------------

use firestream_ci::rundir::{default_build_root, find_repo_root};

/// A tmpdir carrying both repo-root markers plus the nested subdirs a caller
/// might realistically be sitting in.
fn stub_repo_root(tmp: &Path) -> std::path::PathBuf {
    let root = tmp.join("repo");
    std::fs::create_dir_all(root.join("src/containers/firestream")).expect("mkdir markers");
    std::fs::write(root.join("flake.nix"), b"{ }").expect("write flake.nix");
    std::fs::create_dir_all(root.join("src/util/firestream-ci/src")).expect("mkdir nested");
    // A decoy: a nested dir with only ONE of the two markers must not match.
    std::fs::create_dir_all(root.join("src/util/src/containers/firestream")).expect("mkdir decoy");
    std::fs::canonicalize(&root).expect("canonicalize repo root")
}

#[test]
fn find_repo_root_is_depth_independent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = stub_repo_root(tmp.path());

    for rel in [
        ".",
        "src",
        "src/util",
        "src/util/firestream-ci",
        "src/util/firestream-ci/src",
    ] {
        let start = root.join(rel);
        assert_eq!(
            find_repo_root(&start).as_deref(),
            Some(root.as_path()),
            "starting from {rel}, the repo root must resolve to the marker dir, not the cwd"
        );
    }
}

#[test]
fn find_repo_root_requires_both_markers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = stub_repo_root(tmp.path());

    // `src/util/` has `src/containers/firestream` but no `flake.nix`, so the
    // walk must pass straight through it to the real root.
    assert_eq!(
        find_repo_root(&root.join("src/util")).as_deref(),
        Some(root.as_path()),
        "a dir with only one marker must not be mistaken for the repo root"
    );
}

#[test]
fn find_repo_root_is_none_outside_a_repo() {
    // Skipped in-container, where the `/workspace` fallback legitimately fires.
    if Path::new("/workspace/src/containers/firestream").is_dir() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let bare = tmp.path().join("no-markers-here");
    std::fs::create_dir_all(&bare).expect("mkdir");
    assert!(
        find_repo_root(&bare).is_none(),
        "no markers and no /workspace ⇒ no repo root; the caller picks the fallback"
    );
}

#[test]
fn default_build_root_is_absolute_and_named_build() {
    let base = default_build_root();
    assert_eq!(
        base.file_name().and_then(|n| n.to_str()),
        Some("_build"),
        "the build root is always `<root>/_build`"
    );
    assert!(
        base.is_absolute(),
        "must be absolute so a later chdir can't repoint it: {}",
        base.display()
    );
}

/// The actual bug, end to end: this test binary runs with its cwd inside
/// `src/util/`, and the build root it resolves must still be the repo's.
#[test]
fn default_build_root_escapes_the_src_util_workspace() {
    let cwd = std::env::current_dir().expect("cwd");
    let Some(root) = find_repo_root(&cwd) else {
        return; // built outside a checkout (e.g. a Nix sandbox) — nothing to assert
    };
    assert_eq!(
        default_build_root(),
        root.join("_build"),
        "the build root must be the repo's, never `<cwd>/_build`"
    );
    assert!(
        !default_build_root().starts_with(root.join("src/util")),
        "no second `_build/` tree under src/util"
    );
}
