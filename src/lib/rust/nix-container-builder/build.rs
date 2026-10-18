//! Build script for nix-container-builder
//!
//! This script embeds the Nix workspace files at compile time using workspace-embed.

use std::path::PathBuf;
use workspace_embed::EmbedBuilder;

/// Directories embedded into `embedded/`, relative to the repo root.
///
/// This list is the single source of truth: it drives both the `include_dir`
/// calls below AND the `cargo:rerun-if-changed` lines. Those two must never
/// drift — a path included but not watched produces an `embedded/` tree that
/// is silently stale relative to the working tree, and the resulting image is
/// built from whatever the sources looked like the last time cargo happened to
/// re-run this script.
const EMBEDDED_DIRS: &[&str] = &[
    // Nix module system
    "bin/nix/firestream",
    // Container definitions
    "src/containers/firestream",
];

/// Files embedded into `embedded/`, relative to the repo root. Same
/// single-source-of-truth contract as [`EMBEDDED_DIRS`].
const EMBEDDED_FILES: &[&str] = &["flake.nix", "flake.lock"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);

    // Navigate to repo root (4 levels up from src/lib/rust/nix-container-builder)
    let repo_root = manifest_dir
        .ancestors()
        .nth(4)
        .expect("Could not find repo root")
        .to_path_buf();

    println!("cargo:warning=Repo root: {}", repo_root.display());

    // Watch every embedded source. Cargo scans directories recursively, so
    // editing e.g. src/containers/firestream/odoo/scripts/config.sh re-runs
    // this script and regenerates embedded/. Without these lines the only
    // trigger is build.rs itself, so embedded/ goes stale after any container
    // or Nix-module edit and nothing reports it.
    for path in EMBEDDED_DIRS.iter().chain(EMBEDDED_FILES) {
        println!("cargo:rerun-if-changed={}", repo_root.join(path).display());
    }

    let mut builder = EmbedBuilder::new()
        .source_root(&repo_root)
        .output_dir(manifest_dir.join("embedded"));

    for dir in EMBEDDED_DIRS {
        builder = builder.include_dir(dir);
    }
    for file in EMBEDDED_FILES {
        builder = builder.include_file(file);
    }

    let result = builder
        // Respect ignore files
        .respect_gitignore(true)
        .respect_dockerignore(true)
        // Include minimal .git for Nix flake resolution
        .git_minimal()
        // Standard exclusions (in addition to gitignore)
        .exclude("*.pyc")
        .exclude("__pycache__")
        .exclude("node_modules")
        .exclude("target")
        .exclude(".venv")
        .exclude("result")
        .exclude("result-*")
        .build()?;

    println!(
        "cargo:warning=Embedded {} files ({} bytes) to {}",
        result.file_count,
        result.total_size,
        result.output_dir.display()
    );

    Ok(())
}
