//! Asserts the crate's declared rust-version matches the toolchain pinned
//! upstream. Update both `[workspace.package].rust-version` AND this test
//! when bumping MSRV.

#[test]
fn rust_version_matches_workspace_pin() {
    assert_eq!(env!("CARGO_PKG_RUST_VERSION"), "1.82");
}
