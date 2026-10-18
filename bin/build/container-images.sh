#!/usr/bin/env bash
# container-images.sh - Build container images via Nix
#
# Builds natively against the host /nix/store when possible, and falls back to
# a nixos/nix Docker builder with a volume-based Nix cache otherwise (macOS,
# cross-arch target, no nix on PATH, or running inside a container).
# Supports git worktrees by mounting git directories at original paths.
#
# Usage:
#   ./bin/build/container-images.sh <container1> [container2] ...
#   ./bin/build/container-images.sh postgresql --version 17
#   ./bin/build/container-images.sh airflow kafka spark
#
# ── STRANGLER STATUS (Phase 7) ───────────────────────────────────────
# THIS SCRIPT IS STILL THE DEFAULT AND IS UNCHANGED BELOW THIS HEADER.
# `firestream-ci build images` is a second, opt-in implementation of the same
# command line (`--rust`, or FIRESTREAM_BUILD_IMPL=rust, or `make <t> IMPL=rust`).
#
# DELETION CHECKLIST — every box must be ticked before this file is removed:
#   [ ] `FIRESTREAM_BUILD_IMPL=rust make redis-build` completes on a LINUX host
#       and `docker images` shows the same tag the bash path produces.
#   [ ] The same, native strategy, for one heavyweight image (airflow or spark).
#   [ ] The same on a DARWIN host, where the strategy resolves to `docker`, and
#       the builder mounts firestream-nix-store-<arch> (NOT -x86_64).
#   [ ] `FIRESTREAM_BUILD_IMPL=rust make manifest` produces a _build/manifest/
#       byte-identical to the bash path's.
#   [ ] A git-worktree checkout has been exercised on the Docker strategy (the
#       `-v <gitdir>:<gitdir>:ro` pair must appear; `--dry-run` shows it).
#   [ ] Ctrl-C during a multi-package batch leaves no `.build-batch.lock`.
#   [ ] `bin/build/test-registry-parity.sh` and `bin/build/test-strategy-parity.sh`
#       are green (they are the two tables the Rust path reproduces).
#
# THEN, and only then:
#   - makefile: BUILD_CONTAINER / MANIFEST_SCRIPT point at `firestream-ci build ...`
#     and the IMPL selector goes away.
#   - bin/build-container.sh: re-point or delete.
#   - bin/build/_common.sh: only `find_repo_root`, the log_* helpers,
#     CONTAINER_REGISTRY and fs_docker_resources live here; all four have typed
#     equivalents. Delete the file, but KEEP bin/build/strategy.sh — it is
#     sourced from /nix/store by nix/flake-modules/docker-build.nix, where no
#     Rust workspace exists, and it is half the strategy parity gate.
#   - bin/build/registry-cases.json must survive: it is what the Nix profile
#     reads. Its bash-side harness (test-registry-parity.sh) dies with _common.sh.
#
# Copyright Firestream. MIT License.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ── Implementation selector (opt-in; default is this script) ──────────
# Handled before _common.sh is sourced so the Rust path pays none of the
# preamble. `--bash` wins over the env var, so an operator can pin the proven
# path in one invocation without unsetting anything.
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
        exec firestream-ci build images ${_fs_fwd[@]+"${_fs_fwd[@]}"}
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
    printf "${BOLD}Usage:${RESET} $0 [options] <container1> [container2] ...\n"
    echo ""
    echo "  Build Firestream container images using Nix."
    echo "  Uses the host /nix/store when possible; otherwise a Docker builder."
    echo ""
    printf "${BOLD}Containers:${RESET}\n"
    list_containers | sed 's/^/    /'
    echo ""
    printf "${BOLD}Options:${RESET}\n"
    printf "  ${CYAN}--target <arch>${RESET}     Target architecture (x86_64, aarch64)\n"
    printf "  ${CYAN}--version <ver>${RESET}     Container version (applies to next container)\n"
    printf "  ${CYAN}--native${RESET}            Force a native build against the host /nix/store\n"
    printf "  ${CYAN}--docker${RESET}            Force the nixos/nix Docker builder\n"
    printf "  ${CYAN}--rust${RESET}              Run via \`firestream-ci build images\` (opt-in; same args)\n"
    printf "  ${CYAN}--bash${RESET}              Force this script (default; overrides FIRESTREAM_BUILD_IMPL)\n"
    printf "  ${CYAN}--help${RESET}              Show this help\n"
    echo ""
    printf "${BOLD}Examples:${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 airflow                    ${DIM}# Build single container${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 postgresql --version 17    ${DIM}# Build specific version${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 airflow kafka spark        ${DIM}# Build multiple containers${RESET}\n"
    printf "  ${DIM}\$${RESET} $0 --target x86_64 airflow    ${DIM}# Cross-compile for x86_64${RESET}\n"
    echo ""
    exit 1
}

# ── Parse Arguments ──────────────────────────────────────────────────
CONTAINERS=()
VERSIONS=()
current_version=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -lt 2 ]] && { log_error "--version requires argument"; exit 1; }
            current_version="$2"
            shift 2
            ;;
        --target)
            [[ $# -lt 2 ]] && { log_error "--target requires argument"; exit 1; }
            TARGET_ARCH="$2"
            shift 2
            ;;
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
            CONTAINERS+=("$1")
            VERSIONS+=("$current_version")
            current_version=""
            shift
            ;;
    esac
done

[[ ${#CONTAINERS[@]} -eq 0 ]] && usage

# Validate containers exist
for container in "${CONTAINERS[@]}"; do
    if [[ ! -d "$CONTAINERS_DIR/$container" ]]; then
        log_error "Container '$container' not found in $CONTAINERS_DIR"
        echo ""
        echo "Available containers:"
        list_containers | sed 's/^/  /'
        exit 1
    fi
done

# Resolve all package names
PACKAGES=()
for i in "${!CONTAINERS[@]}"; do
    pkg=$(resolve_package_name "${CONTAINERS[$i]}" "${VERSIONS[$i]}")
    PACKAGES+=("$pkg")
done

log_info "Building: ${PACKAGES[*]} (arch: $TARGET_ARCH)"

# ── Setup ────────────────────────────────────────────────────────────
mkdir -p "$BUILD_OUTPUT_DIR"

# Acquire batch lock to prevent concurrent builds
BATCH_LOCK="$BUILD_OUTPUT_DIR/.build-batch.lock"
INTERRUPTED=false
BUILD_PID=""

cleanup() {
    if [[ -n "$BUILD_PID" ]] && kill -0 "$BUILD_PID" 2>/dev/null; then
        kill "$BUILD_PID" 2>/dev/null || true
        wait "$BUILD_PID" 2>/dev/null || true
    fi
    rm -rf "$BATCH_LOCK"
}

handle_interrupt() {
    INTERRUPTED=true
    echo ""
    log_warn "Interrupted - stopping builds..."
    cleanup
    exit 130
}

trap cleanup EXIT
trap handle_interrupt INT TERM

if ! mkdir "$BATCH_LOCK" 2>/dev/null; then
    lock_pid=$(cat "$BATCH_LOCK/pid" 2>/dev/null || echo "")
    if [[ -n "$lock_pid" ]] && kill -0 "$lock_pid" 2>/dev/null; then
        log_error "Another build is running (PID $lock_pid)"
        log_error "If stale, remove: rm -rf $BATCH_LOCK"
        exit 1
    fi
    log_warn "Removing stale lock (PID ${lock_pid:-unknown})"
    rm -rf "$BATCH_LOCK"
    mkdir "$BATCH_LOCK"
fi
echo $$ > "$BATCH_LOCK/pid"

# ── Build each container ─────────────────────────────────────────────
BUILD_START=$(date +%s)
SUCCEEDED=0
FAILED=0
FAILED_CONTAINERS=()

for i in "${!PACKAGES[@]}"; do
    # Check for interrupt before starting next build
    if $INTERRUPTED; then
        log_warn "Skipping remaining builds due to interrupt"
        break
    fi

    pkg="${PACKAGES[$i]}"
    container="${CONTAINERS[$i]}"
    OUT_DIR="$BUILD_OUTPUT_DIR/$pkg"
    mkdir -p "$OUT_DIR"

    echo ""
    log_info "Building $pkg ($((i+1))/${#PACKAGES[@]})..."

    # Dispatches native vs docker via strategy.sh. Process substitution keeps
    # the build's exit code while still teeing output to the per-package log.
    fs_build_image "$REPO_ROOT" ".#$pkg" "$OUT_DIR/${pkg}.tar.gz" "$TARGET_ARCH" --sock \
        > >(tee "$OUT_DIR/build.log") 2>&1 &
    BUILD_PID=$!
    wait $BUILD_PID
    build_status=$?
    BUILD_PID=""

    if $INTERRUPTED; then
        break
    fi

    # Verify output file exists on host (defense in depth)
    if [[ $build_status -eq 0 ]] && [[ ! -s "$OUT_DIR/${pkg}.tar.gz" ]]; then
        log_error "Build reported success but output file missing or empty: $OUT_DIR/${pkg}.tar.gz"
        build_status=1
    fi

    if [[ $build_status -eq 0 ]]; then
        rm -f "$OUT_DIR/build.log"  # Remove log on success

        # Load into Docker
        log_step "Loading $pkg into Docker..."
        LOAD_OUTPUT=$(docker load < "$OUT_DIR/${pkg}.tar.gz" 2>&1)
        if [[ $? -eq 0 ]]; then
            IMAGE_TAG=$(echo "$LOAD_OUTPUT" | sed -n 's/.*Loaded image: //p' | awk '{print $1}' | tail -1)
            [[ -z "$IMAGE_TAG" ]] && IMAGE_TAG="(unknown)"
            log_ok "$pkg -> $IMAGE_TAG"
            SUCCEEDED=$((SUCCEEDED + 1))
        else
            log_error "Failed to load $pkg into Docker"
            FAILED=$((FAILED + 1))
            FAILED_CONTAINERS+=("$pkg")
        fi
    else
        log_error "FAILED: $pkg (see $OUT_DIR/build.log)"
        FAILED=$((FAILED + 1))
        FAILED_CONTAINERS+=("$pkg")
    fi
done

BUILD_END=$(date +%s)
BUILD_DURATION=$((BUILD_END - BUILD_START))

# ── Summary ──────────────────────────────────────────────────────────
echo ""
printf "${BOLD}${CYAN}"
echo "  ╔══════════════════════════════════════════════════╗"
printf "  ║  %-48s║\n" "BUILD SUMMARY"
echo "  ╠══════════════════════════════════════════════════╣"
if [[ "$FAILED" -eq 0 ]]; then
    printf "  ║  ${GREEN}Succeeded${CYAN}   %-38s║\n" "$SUCCEEDED"
else
    printf "  ║  ${GREEN}Succeeded${CYAN}   %-38s║\n" "$SUCCEEDED"
    printf "  ║  ${RED}Failed${CYAN}      %-38s║\n" "$FAILED"
fi
printf "  ║  Duration    %-38s║\n" "${BUILD_DURATION}s"
echo "  ╚══════════════════════════════════════════════════╝"
printf "${RESET}\n"

if [[ ${#FAILED_CONTAINERS[@]} -gt 0 ]]; then
    log_error "Failed containers: ${FAILED_CONTAINERS[*]}"
    exit 1
fi

exit 0
