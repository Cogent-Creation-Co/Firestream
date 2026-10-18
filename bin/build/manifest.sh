#!/usr/bin/env bash
# manifest.sh - Build fleet SBOM manifest via Nix
#
# Builds natively against the host /nix/store when possible, falling back to a
# nixos/nix Docker builder with a volume-based Nix cache otherwise.
# Supports git worktrees by mounting git directories at original paths.
#
# Usage:
#   ./bin/build/manifest.sh              # Build fleet manifest
#   ./bin/build/manifest.sh airflow      # Build individual SBOM
#
# ── STRANGLER STATUS (Phase 7) ───────────────────────────────────────
# THIS SCRIPT IS STILL THE DEFAULT AND IS UNCHANGED BELOW THIS HEADER.
# `firestream-ci build manifest` is a second, opt-in implementation of the same
# command line (`--rust`, or FIRESTREAM_BUILD_IMPL=rust, or `make manifest
# IMPL=rust`). The deletion checklist for BOTH scripts lives in the header of
# bin/build/container-images.sh.
#
# Copyright Firestream. MIT License.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ── Implementation selector (opt-in; default is this script) ──────────
# See the identical block in container-images.sh.
_fs_impl="${FIRESTREAM_BUILD_IMPL:-bash}"
_fs_fwd=()
for _arg in "$@"; do
    case "$_arg" in
        --rust) _fs_impl="rust" ;;
        --bash) _fs_impl="bash" ;;
        *)      _fs_fwd+=("$_arg") ;;
    esac
done
case "$_fs_impl" in
    rust)
        if ! command -v firestream-ci >/dev/null 2>&1; then
            echo "FIRESTREAM_BUILD_IMPL=rust but firestream-ci is not on PATH." >&2
            echo "Enter the devshell (nix develop) or build it:" >&2
            echo "  cd src/util && cargo build --release -p firestream-ci" >&2
            exit 1
        fi
        exec firestream-ci build manifest ${_fs_fwd[@]+"${_fs_fwd[@]}"}
        ;;
    bash) ;;
    *)
        echo "  ! Unknown FIRESTREAM_BUILD_IMPL='${_fs_impl}' (expected bash|rust); using bash" >&2
        ;;
esac
set -- ${_fs_fwd[@]+"${_fs_fwd[@]}"}

source "$SCRIPT_DIR/_common.sh"

TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"

# ── Usage ────────────────────────────────────────────────────────────
usage() {
    echo ""
    printf "${BOLD}Usage:${RESET} $0 [container]\n"
    echo ""
    echo "  Build fleet SBOM manifest using Nix."
    echo "  Uses the host /nix/store when possible; otherwise a Docker builder."
    echo ""
    printf "${BOLD}Commands:${RESET}\n"
    printf "  ${CYAN}(no args)${RESET}     Build complete fleet manifest\n"
    printf "  ${CYAN}<container>${RESET}   Build SBOM for specific container\n"
    echo ""
    printf "${BOLD}Options:${RESET}\n"
    printf "  ${CYAN}--native${RESET}      Force a native build against the host /nix/store\n"
    printf "  ${CYAN}--docker${RESET}      Force the nixos/nix Docker builder\n"
    printf "  ${CYAN}--rust${RESET}        Run via \`firestream-ci build manifest\` (opt-in; same args)\n"
    printf "  ${CYAN}--bash${RESET}        Force this script (default; overrides FIRESTREAM_BUILD_IMPL)\n"
    echo ""
    printf "${BOLD}Examples:${RESET}\n"
    printf "  ${DIM}\$${RESET} $0                    ${DIM}# Build fleet manifest${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 airflow            ${DIM}# Build airflow SBOM only${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 spark              ${DIM}# Build spark SBOM only${RESET}\n"
    echo ""
    exit 1
}

# ── Parse Arguments ──────────────────────────────────────────────────
CONTAINER=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --native)
            export FIRESTREAM_BUILD_STRATEGY="native"
            shift
            ;;
        --docker)
            export FIRESTREAM_BUILD_STRATEGY="docker"
            shift
            ;;
        --help|-h)
            usage
            ;;
        -*)
            log_error "Unknown option: $1"
            exit 1
            ;;
        *)
            CONTAINER="$1"
            shift
            ;;
    esac
done

# Determine what to build
if [[ -z "$CONTAINER" ]]; then
    NIX_TARGET=".#manifest"
    OUTPUT_NAME="manifest"
    log_info "Building fleet manifest"
else
    NIX_TARGET=".#sbom.$CONTAINER"
    OUTPUT_NAME="sbom-$CONTAINER"
    log_info "Building SBOM for: $CONTAINER"
fi

# ── Setup ────────────────────────────────────────────────────────────
OUTPUT_DIR="$BUILD_OUTPUT_DIR/$OUTPUT_NAME"
mkdir -p "$BUILD_OUTPUT_DIR"

# ── Build ────────────────────────────────────────────────────────────
BUILD_START=$(date +%s)

# Dispatches native vs docker via strategy.sh (--dir: directory output).
fs_build_image "$REPO_ROOT" "$NIX_TARGET" "$OUTPUT_DIR" "$TARGET_ARCH" --dir

build_status=$?
BUILD_END=$(date +%s)
BUILD_DURATION=$((BUILD_END - BUILD_START))

if [[ $build_status -eq 0 ]]; then
    echo ""
    log_ok "Build completed in ${BUILD_DURATION}s"
    log_step "Output: $OUTPUT_DIR"
    echo ""
    ls -la "$OUTPUT_DIR/"
else
    log_error "Build failed after ${BUILD_DURATION}s"
    exit 1
fi
