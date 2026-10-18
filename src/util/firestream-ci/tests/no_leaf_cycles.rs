//! Enforces: `firestream-otel-cli` and `firestream-nix-build` MUST NOT depend on
//! `firestream-ci`. See plan §"Dep direction rule (leaf vs non-leaf)".
//!
//! `cargo deny` cannot natively express "crate X must not appear in the
//! dep tree of crate Y", so we shell out to `cargo tree -p <leaf>` which
//! resolves dependencies *scoped to the named package* — unlike
//! `cargo metadata`, which lists all workspace members regardless of
//! whether the leaf actually depends on them.

fn leaf_does_not_depend_on_firestream_ci(manifest_relpath: &str, leaf_name: &str) {
    let output = std::process::Command::new("cargo")
        .args([
            "tree",
            "--manifest-path",
            manifest_relpath,
            "-p",
            leaf_name,
            "--prefix",
            "none",
            "--edges",
            "normal,build,dev",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree failed to spawn");
    assert!(
        output.status.success(),
        "cargo tree exited non-zero for {leaf_name}:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    // Match the package name as a token (word-boundary delimited) so we
    // don't accidentally match a future crate name like `firestream-ci-something`.
    let depends_on_firestream_ci = tree
        .lines()
        .any(|line| line.split_whitespace().next() == Some("firestream-ci"));
    assert!(
        !depends_on_firestream_ci,
        "{leaf_name} must not depend on firestream-ci (leaf-vs-non-leaf rule violated).\n\nFull dep tree:\n{tree}"
    );
}

#[test]
fn otel_cli_does_not_depend_on_firestream_ci() {
    // Package name is `firestream-otel-cli`; its [lib]/[bin] names stay
    // `otel_cli` / `otel-cli` to match the Go upstream command-for-command.
    leaf_does_not_depend_on_firestream_ci(
        "../firestream-otel-cli/Cargo.toml",
        "firestream-otel-cli",
    );
}

#[test]
fn firestream_nix_build_does_not_depend_on_firestream_ci() {
    leaf_does_not_depend_on_firestream_ci("../firestream-nix-build/Cargo.toml", "firestream-nix-build");
}
