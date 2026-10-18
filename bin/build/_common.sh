#!/usr/bin/env bash
# _common.sh - shared preamble for Firestream build scripts
#
# Provides:
#   - Repo root detection (works in worktrees)
#   - Git worktree mount resolution for Docker
#   - Logging utilities
#   - Docker resource detection
#   - Container registry mapping
#
# Copyright Firestream. MIT License.

set -euo pipefail

# ── Build Strategy Library ───────────────────────────────────────────
# Side-effect-free; also sourced by nix/flake-modules/docker-build.nix.
# Provides: resolve_physical, resolve_git_mounts, get_nix_volume,
#           fs_norm_arch, fs_host_arch, fs_in_container, fs_native_blocker,
#           fs_choose_strategy, fs_nix_build_native, fs_nix_build_docker,
#           fs_build_image
# shellcheck source=bin/build/strategy.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/strategy.sh"

# ── Path Resolution ──────────────────────────────────────────────────
# Find repo root - works in containers and worktrees
find_repo_root() {
    local dir="${1:-$(pwd)}"
    dir="$(resolve_physical "$dir")"

    while [[ "$dir" != "/" ]]; do
        # Check for Firestream marker files (flake.nix + containers dir)
        if [[ -f "$dir/flake.nix" && -d "$dir/src/containers/firestream" ]]; then
            echo "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done

    # Fallback: check common container mount point
    if [[ -d "/workspace/src/containers/firestream" ]]; then
        echo "/workspace"
        return 0
    fi

    echo "ERROR: Could not find repo root" >&2
    return 1
}

# ── Initialize paths ─────────────────────────────────────────────────
# SCRIPT_DIR should be set by sourcing script, fallback to current
SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd -P)}"
REPO_ROOT="$(find_repo_root "$SCRIPT_DIR")"
CONTAINERS_DIR="$REPO_ROOT/src/containers/firestream"
BUILD_OUTPUT_DIR="${BUILD_OUTPUT_DIR:-$REPO_ROOT/_build}"
BUILDER_IMAGE_NAME="firestream-devcontainer"

export SCRIPT_DIR REPO_ROOT CONTAINERS_DIR BUILD_OUTPUT_DIR BUILDER_IMAGE_NAME

# ── Logging ──────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    BOLD='\033[1m' DIM='\033[2m' RESET='\033[0m'
    RED='\033[31m' GREEN='\033[32m' YELLOW='\033[33m' BLUE='\033[34m' CYAN='\033[36m'
else
    BOLD='' DIM='' RESET='' RED='' GREEN='' YELLOW='' BLUE='' CYAN=''
fi

log_info()  { printf "${DIM}%s${RESET} ${CYAN}>>>${RESET} %s\n" "$(date +%H:%M:%S)" "$*"; }
log_step()  { printf "${DIM}%s${RESET} ${BLUE} ->${RESET} %s\n" "$(date +%H:%M:%S)" "$*"; }
log_ok()    { printf "${DIM}%s${RESET} ${GREEN}  ✓${RESET} %s\n" "$(date +%H:%M:%S)" "$*"; }
log_warn()  { printf "${DIM}%s${RESET} ${YELLOW}  !${RESET} %s\n" "$(date +%H:%M:%S)" "$*"; }
log_error() { printf "${DIM}%s${RESET} ${RED}  ✗${RESET} %s\n" "$(date +%H:%M:%S)" "$*" >&2; }

# ── Docker Resource Detection ────────────────────────────────────────
_detect_docker_cpus() {
    docker info --format '{{.NCPU}}' 2>/dev/null || nproc 2>/dev/null || echo 4
}

_detect_docker_memory() {
    local bytes
    bytes=$(docker info --format '{{.MemTotal}}' 2>/dev/null) || bytes=0
    if [[ "$bytes" -gt 0 ]]; then
        local gb=$(( bytes / 1073741824 ))
        local usable=$(( gb > 1 ? gb - 1 : 1 ))  # Reserve 1GB overhead
        echo "${usable}g"
    else
        echo "8g"
    fi
}

# Lazily populate DOCKER_CPUS / DOCKER_MEMORY / DOCKER_SWAP.
# NOT done at source time: `docker info` is two round-trips (and a multi-second
# stall plus noise on a host with no daemon) that the native build path never
# needs. Called from fs_nix_build_docker in strategy.sh.
fs_docker_resources() {
    if [[ -n "${_FS_DOCKER_RESOURCES_DONE:-}" ]]; then return 0; fi
    DOCKER_CPUS="${DOCKER_CPUS:-$(_detect_docker_cpus)}"
    DOCKER_MEMORY="${DOCKER_MEMORY:-$(_detect_docker_memory)}"
    DOCKER_SWAP="${DOCKER_SWAP:-$(( ${DOCKER_MEMORY%g} * 2 ))g}"
    export DOCKER_CPUS DOCKER_MEMORY DOCKER_SWAP
    _FS_DOCKER_RESOURCES_DONE=1
}

# ── Container Registry ───────────────────────────────────────────────
# Maps container:version to Nix package name
declare -A CONTAINER_REGISTRY=(
    ["postgresql:16"]="postgresql-16"
    ["postgresql:17"]="postgresql-17"
    ["postgresql:"]="postgresql-17"
    ["redis:7"]="redis-7"
    ["redis:8"]="redis-8"
    ["redis:"]="redis-7"
    ["superset:4"]="superset-4"
    ["superset:5"]="superset-5"
    ["superset:"]="superset-5"
    ["airflow:3"]="airflow-3"
    ["airflow:"]="airflow-3"
    ["kafka:4"]="kafka-4"
    ["kafka:"]="kafka-4"
    ["spark:4"]="spark-4"
    ["spark:"]="spark-4"
    ["jupyterhub:5"]="jupyterhub-5"
    ["jupyterhub:"]="jupyterhub-5"
    ["odoo:15"]="odoo-15"
    ["odoo:16"]="odoo-16"
    ["odoo:17"]="odoo-17"
    ["odoo:18"]="odoo-18"
    ["odoo:"]="odoo"
)

resolve_package_name() {
    local container="$1"
    local version="${2:-}"
    local key="${container}:${version}"

    # Try exact match first
    if [[ -n "${CONTAINER_REGISTRY[$key]:-}" ]]; then
        echo "${CONTAINER_REGISTRY[$key]}"
        return 0
    fi

    # Try default version
    if [[ -n "${CONTAINER_REGISTRY[${container}:]:-}" ]]; then
        echo "${CONTAINER_REGISTRY[${container}:]}"
        return 0
    fi

    log_error "Unknown container: $container (version: ${version:-default})"
    return 1
}

# List available containers
list_containers() {
    if [[ -d "$CONTAINERS_DIR" ]]; then
        ls -1 "$CONTAINERS_DIR" 2>/dev/null | grep -v "^\."
    fi
}
