//! End-to-end fixtures for `worktree::Worktree::container_mounts`.
//! Gated behind `BUILDER_E2E=1` because each test shells out to `git
//! init` / `git worktree add` to materialize real on-disk state.
//!
//! When unset, the test prints a skip notice and returns success — same
//! convention used elsewhere in the workspace for E2E-flagged tests.
//! Run with: `BUILDER_E2E=1 cargo test -p firestream-ci --test integration_worktree`.

use std::path::Path;
use std::process::Command;

use firestream_ci::worktree::{MountKind, Worktree};

fn e2e_enabled() -> bool {
    std::env::var("BUILDER_E2E").as_deref() == Ok("1")
}

fn regen(fixtures_root: &Path) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("regen_worktree.sh");
    let status = Command::new("bash")
        .arg(&script)
        .arg(fixtures_root)
        .status()
        .expect("spawn regen_worktree.sh");
    assert!(status.success(), "regen_worktree.sh exited non-zero");
}

fn fresh_fixtures() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    regen(dir.path());
    dir
}

#[test]
fn plain_repo_mounts_workdir_and_gitdir() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let wt = Worktree::open(fix.path().join("plain_repo")).unwrap();
    let mounts = wt.container_mounts().unwrap();
    // Plain repo: workdir bind + gitdir bind. No symlink mounts.
    assert!(mounts.iter().all(|m| m.kind == MountKind::Bind));
    assert!(
        mounts.len() >= 2,
        "got {} mounts: {:?}",
        mounts.len(),
        mounts
    );
    assert!(!wt.is_worktree());
}

#[test]
fn single_worktree_mounts_commondir_separately() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let wt = Worktree::open(fix.path().join("single_worktree").join("wt")).unwrap();
    assert!(wt.is_worktree());
    let mounts = wt.container_mounts().unwrap();
    // Worktree gitdir + parent commondir must both appear. The parent
    // commondir is the `.git` of the parent repo, which is a different
    // path than the worktree's gitdir pointer.
    let parent_git = fix
        .path()
        .join("single_worktree")
        .join("parent")
        .join(".git");
    let parent_git_canon = std::fs::canonicalize(parent_git).unwrap();
    assert!(
        mounts.iter().any(|m| m.source == parent_git_canon),
        "parent .git not mounted; got {:?}",
        mounts
    );
}

#[test]
fn nested_worktree_resolves_both_levels() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let wt = Worktree::open(fix.path().join("nested_worktree").join("wt2")).unwrap();
    assert!(wt.is_worktree());
    let mounts = wt.container_mounts().unwrap();
    // Commondir must still resolve to the root parent's .git, not wt1's
    // (worktree gitdirs chain through commondir pointers).
    let root_git = fix
        .path()
        .join("nested_worktree")
        .join("parent")
        .join(".git");
    let root_git_canon = std::fs::canonicalize(root_git).unwrap();
    assert!(
        mounts.iter().any(|m| m.source == root_git_canon),
        "root .git not mounted; got {:?}",
        mounts
    );
}

#[test]
fn detached_head_reports_no_branch() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let wt = Worktree::open(fix.path().join("detached_head")).unwrap();
    assert_eq!(wt.head_branch(), None);
    // Container mounts still resolve (detached HEAD is a normal repo).
    let mounts = wt.container_mounts().unwrap();
    assert!(!mounts.is_empty());
}

#[test]
fn submodule_repo_opens_and_mounts() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let super_path = fix.path().join("submodule").join("super");
    let wt = Worktree::open(&super_path).unwrap();
    let mounts = wt.container_mounts().unwrap();
    // Super's .git/modules/sub directory must be in the mount set.
    let modules_dir = std::fs::canonicalize(super_path.join(".git").join("modules")).unwrap();
    let has_submodule_mount = mounts.iter().any(|m| m.source.starts_with(&modules_dir));
    assert!(
        has_submodule_mount,
        "no submodule mount found; got {:?}",
        mounts
    );
}

#[test]
fn symlink_path_emits_symlink_bind() {
    if !e2e_enabled() {
        eprintln!("skipped (set BUILDER_E2E=1 to enable)");
        return;
    }
    let fix = fresh_fixtures();
    let link_path = fix.path().join("symlink_divergent").join("link");
    let real_path = fix.path().join("symlink_divergent").join("real");
    // Open via the symlinked path so the logical and physical paths
    // differ at the final component.
    let wt = Worktree::open(&link_path).unwrap();
    assert_ne!(wt.logical_path(), wt.physical_path());
    let mounts = wt.container_mounts().unwrap();
    // Must produce at least one SymlinkBind whose target is rooted at
    // the logical path and whose source is rooted at the physical path.
    let real_canon = std::fs::canonicalize(&real_path).unwrap();
    let has_symlink_mount = mounts.iter().any(|m| {
        m.kind == MountKind::SymlinkBind
            && (m.source.starts_with(&real_canon) || m.source == real_canon)
            && (m.target == link_path
                || m.target.starts_with(&link_path)
                || m.target.starts_with(link_path.parent().unwrap()))
    });
    assert!(
        has_symlink_mount,
        "no SymlinkBind mount found between link={} and real={}; got {:?}",
        link_path.display(),
        real_canon.display(),
        mounts
    );
}
