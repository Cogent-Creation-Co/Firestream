#!/usr/bin/env bash
# Regenerate the worktree fixture tree. Idempotent: nukes + rebuilds.
# Six fixtures cover the cases the libgit2 symlink-divergence walk must
# handle:
#
#   plain_repo/            : git init, single tree.
#   single_worktree/       : parent + one `git worktree add`. .git is a
#                            pointer file; commondir lives in parent's
#                            `.git/worktrees/...`.
#   nested_worktree/       : worktree of a worktree.
#   detached_head/         : plain repo with HEAD detached at the initial
#                            commit (head_branch() must return None).
#   submodule/             : superproject with one submodule. The
#                            submodule's `.git` is a pointer to the
#                            super's `.git/modules/<name>/`.
#   symlink_divergent/     : a workdir reached via a symlink chain whose
#                            canonical path differs from the symlink path.
#
# These are *not* checked into the repo as live git state — they're built
# on demand by this script. The integration test calls this script and
# inspects the resulting paths.

set -euo pipefail

ROOT="${1:?Usage: $0 <output-dir>}"
mkdir -p "$ROOT"

build_plain_repo() {
    local d="$ROOT/plain_repo"
    rm -rf "$d"
    mkdir -p "$d"
    git -C "$d" init --quiet --initial-branch=main
    git -C "$d" config user.email ci@firestream.local
    git -C "$d" config user.name ci
    echo "hello" > "$d/README.md"
    git -C "$d" add README.md
    git -C "$d" commit --quiet --no-gpg-sign -m "init"
}

build_single_worktree() {
    local parent="$ROOT/single_worktree/parent"
    local wt="$ROOT/single_worktree/wt"
    rm -rf "$ROOT/single_worktree"
    mkdir -p "$parent"
    git -C "$parent" init --quiet --initial-branch=main
    git -C "$parent" config user.email ci@firestream.local
    git -C "$parent" config user.name ci
    echo "hello" > "$parent/README.md"
    git -C "$parent" add README.md
    git -C "$parent" commit --quiet --no-gpg-sign -m "init"
    git -C "$parent" worktree add -q "$wt" -b feature
}

build_nested_worktree() {
    local parent="$ROOT/nested_worktree/parent"
    local wt1="$ROOT/nested_worktree/wt1"
    local wt2="$ROOT/nested_worktree/wt2"
    rm -rf "$ROOT/nested_worktree"
    mkdir -p "$parent"
    git -C "$parent" init --quiet --initial-branch=main
    git -C "$parent" config user.email ci@firestream.local
    git -C "$parent" config user.name ci
    echo "hello" > "$parent/README.md"
    git -C "$parent" add README.md
    git -C "$parent" commit --quiet --no-gpg-sign -m "init"
    git -C "$parent" worktree add -q "$wt1" -b branch-a
    git -C "$wt1" worktree add -q "$wt2" -b branch-b
}

build_detached_head() {
    local d="$ROOT/detached_head"
    rm -rf "$d"
    mkdir -p "$d"
    git -C "$d" init --quiet --initial-branch=main
    git -C "$d" config user.email ci@firestream.local
    git -C "$d" config user.name ci
    echo "hello" > "$d/README.md"
    git -C "$d" add README.md
    git -C "$d" commit --quiet --no-gpg-sign -m "init"
    git -C "$d" checkout --quiet --detach HEAD
}

build_submodule() {
    local super="$ROOT/submodule/super"
    local sub="$ROOT/submodule/sub_source"
    rm -rf "$ROOT/submodule"
    mkdir -p "$sub"

    git -C "$sub" init --quiet --initial-branch=main
    git -C "$sub" config user.email ci@firestream.local
    git -C "$sub" config user.name ci
    echo "sub" > "$sub/SUB.md"
    git -C "$sub" add SUB.md
    git -C "$sub" commit --quiet --no-gpg-sign -m "sub init"

    mkdir -p "$super"
    git -C "$super" init --quiet --initial-branch=main
    git -C "$super" config user.email ci@firestream.local
    git -C "$super" config user.name ci
    git -C "$super" -c protocol.file.allow=always submodule add -q "$sub" sub
    git -C "$super" commit --quiet --no-gpg-sign -m "add sub"
}

build_symlink_divergent() {
    # Real on-disk repo lives at $ROOT/symlink_divergent/real.
    # Symlinks: $ROOT/symlink_divergent/link  -> real
    # When firestream-ci opens via the link path, canonicalization yields real.
    local d="$ROOT/symlink_divergent"
    rm -rf "$d"
    mkdir -p "$d/real"
    git -C "$d/real" init --quiet --initial-branch=main
    git -C "$d/real" config user.email ci@firestream.local
    git -C "$d/real" config user.name ci
    echo "hello" > "$d/real/README.md"
    git -C "$d/real" add README.md
    git -C "$d/real" commit --quiet --no-gpg-sign -m "init"
    ln -s real "$d/link"
}

build_plain_repo
build_single_worktree
build_nested_worktree
build_detached_head
build_submodule
build_symlink_divergent

echo "fixtures written under $ROOT"
