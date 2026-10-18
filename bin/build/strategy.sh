#!/usr/bin/env bash
# strategy.sh - build-strategy decision predicate + build primitives
#
# THE SINGLE SOURCE OF TRUTH for "should this Nix build run natively on the host
# store, or inside a nixos/nix Docker container with a persistent /nix volume?"
#
# Sourced by:
#   - bin/build/_common.sh              (=> container-images.sh, manifest.sh)
#   - nix/flake-modules/docker-build.nix via `source ${../../bin/build/strategy.sh}`
#
# ── CONTRACT ─────────────────────────────────────────────────────────
# This file MUST remain side-effect free and self-contained:
#   * no `set -e` / `set -u` / `set -o pipefail`
#   * no top-level statements other than function definitions
#   * no dependency on $REPO_ROOT, $SCRIPT_DIR, colour vars, or the log_* helpers
# docker-build.nix sources it from a /nix/store path where none of the above
# exist. Guarded fallbacks for log_* are provided below.
#
# ── ESCAPE HATCH ─────────────────────────────────────────────────────
#   FIRESTREAM_BUILD_STRATEGY=auto|native|docker
# Read at the very top of fs_choose_strategy, before any probing, so that
# `export FIRESTREAM_BUILD_STRATEGY=docker` is a total rollback to the previous
# Docker-only behaviour with no code revert.
#
# ── PARITY ───────────────────────────────────────────────────────────
# The Rust mirror is `firestream_ci::platform` (src/util/firestream-ci/).
# bin/build/strategy-cases.json is the golden-vector file both implementations
# are driven over; bin/build/test-strategy-parity.sh is this side's harness.
# Any change to fs_norm_arch / fs_in_container / fs_native_blocker /
# fs_choose_strategy / fs_docker_platform / get_nix_volume MUST be made in both
# places and reflected in strategy-cases.json.
#
# Known intentional asymmetry: the Rust side additionally consults the CI
# profile's `build_strategy.default` (ci-manifest.json) *below*
# FIRESTREAM_BUILD_STRATEGY and *above* probing. This file is profile-blind,
# which is exactly `build_strategy.default = "auto"`. Golden cases that set a
# non-auto profile default carry an explicit `expect_shell` block recording the
# shell's (profile-blind) answer, so the divergence is asserted, not assumed.
#
# Copyright Firestream. MIT License.

# ── Logging fallbacks ────────────────────────────────────────────────
# Only defined if the sourcing environment has not already provided them.
declare -F log_info  >/dev/null || log_info()  { printf '>>> %s\n' "$*" >&2; }
declare -F log_step  >/dev/null || log_step()  { printf ' -> %s\n' "$*" >&2; }
declare -F log_ok    >/dev/null || log_ok()    { printf '  v %s\n' "$*" >&2; }
declare -F log_warn  >/dev/null || log_warn()  { printf '  ! %s\n' "$*" >&2; }
declare -F log_error >/dev/null || log_error() { printf '  x %s\n' "$*" >&2; }

# ── Path Resolution ──────────────────────────────────────────────────
# Convert paths to physical (resolve symlinks)
resolve_physical() {
    local path="$1"
    if [[ -d "$path" ]]; then
        (cd "$path" && pwd -P)
    elif [[ -f "$path" ]]; then
        local dir
        dir="$(dirname "$path")"
        echo "$(cd "$dir" && pwd -P)/$(basename "$path")"
    else
        echo "$path"
    fi
}

# ── Git Worktree Mount Resolution ────────────────────────────────────
# Parse .git file directly (not via git command which may fail in container)
# Returns Docker mount flags needed for Nix/libgit2 to work
resolve_git_mounts() {
    local repo_root="$1"
    GIT_DOCKER_MOUNTS=()

    # Check if .git is a file (worktree) vs directory (regular repo)
    if [[ -f "$repo_root/.git" ]]; then
        # Parse gitdir from .git file: "gitdir: /path/to/main/.git/worktrees/name"
        local gitdir
        gitdir=$(sed 's/^gitdir: //' "$repo_root/.git" | tr -d '\n\r')

        # Make absolute if relative
        if [[ "$gitdir" != /* ]]; then
            gitdir="$repo_root/$gitdir"
        fi
        gitdir=$(resolve_physical "$gitdir")

        # Read commondir to find main .git directory
        # commondir file contains relative path like "../.."
        local commondir_content main_git_dir
        if [[ -f "$gitdir/commondir" ]]; then
            commondir_content=$(tr -d '\n\r' < "$gitdir/commondir")
            main_git_dir=$(resolve_physical "$gitdir/$commondir_content")
        else
            # Fallback: worktrees dir is inside main .git
            # /path/to/.git/worktrees/name -> /path/to/.git
            main_git_dir=$(resolve_physical "$gitdir/../..")
        fi

        # Mount both the worktree gitdir and main .git at their original paths
        # This allows absolute paths in gitdir pointer to resolve correctly
        GIT_DOCKER_MOUNTS=(
            -v "$gitdir:$gitdir:ro"
            -v "$main_git_dir:$main_git_dir:ro"
        )

        export GIT_WORKTREE_DETECTED="true"
        export GIT_WORKTREE_GITDIR="$gitdir"
        export GIT_WORKTREE_MAIN_GIT="$main_git_dir"
    else
        export GIT_WORKTREE_DETECTED="false"
        export GIT_WORKTREE_GITDIR=""
        export GIT_WORKTREE_MAIN_GIT=""
    fi
}

# ── Architecture-Specific Nix Store Volumes ──────────────────────────
# Separate volumes per architecture to avoid cache pollution
get_nix_volume() {
    local arch="${1:-$(uname -m)}"
    case "$arch" in
        x86_64|amd64)  echo "firestream-nix-store-amd64" ;;
        aarch64|arm64) echo "firestream-nix-store-arm64" ;;
        *)             echo "firestream-nix-store-$arch" ;;
    esac
}

# Map architecture to a Docker --platform string
fs_docker_platform() {
    local arch="${1:-$(uname -m)}"
    case "$arch" in
        x86_64|amd64)  echo "linux/amd64" ;;
        aarch64|arm64) echo "linux/arm64" ;;
        *)             echo "linux/$arch" ;;
    esac
}

# ── Architecture Normalisation ───────────────────────────────────────
fs_norm_arch() {
    case "${1:-}" in
        amd64|x64|x86_64) echo "x86_64" ;;
        arm64|aarch64)    echo "aarch64" ;;
        *)                echo "${1:-}" ;;
    esac
}

fs_host_arch() {
    fs_norm_arch "$(uname -m)"
}

# ── Filesystem probe root (TEST SEAM) ────────────────────────────────
# FIRESTREAM_PROBE_ROOT prefixes the three *absolute filesystem* probes below
# (/.dockerenv, /proc/1/cgroup, /nix/store). It is empty in production, so the
# probes are literally the paths above and behaviour is unchanged.
#
# It exists because those three conditions cannot otherwise be injected by a
# test without root or a mount namespace, and an untestable predicate is not a
# predicate you can hold to a parity contract. The Rust mirror honours the same
# variable in `firestream_ci::platform::Probe::detect` — see
# bin/build/test-strategy-parity.sh and src/util/firestream-ci/src/platform/.
fs_probe_root() {
    printf '%s' "${FIRESTREAM_PROBE_ROOT:-}"
}

# ── Container Detection ──────────────────────────────────────────────
# Mirrors src/lib/rust/nix-container-builder/src/platform.rs::is_running_in_container
fs_in_container() {
    local _r
    _r="$(fs_probe_root)"
    if [[ -f "$_r/.dockerenv" ]]; then return 0; fi
    if [[ -n "${KUBERNETES_SERVICE_HOST:-}" ]]; then return 0; fi
    if [[ -n "${CONTAINER:-}" ]]; then return 0; fi
    if [[ -n "${container:-}" ]]; then return 0; fi
    if [[ -r "$_r/proc/1/cgroup" ]] \
       && grep -qE 'docker|kubepods|containerd' "$_r/proc/1/cgroup" 2>/dev/null; then
        return 0
    fi
    return 1
}

# ── Native Availability ──────────────────────────────────────────────
# Echoes a human-readable reason why a native build is NOT possible, or an
# empty string when native IS possible. Returning a *string* rather than
# logging is what keeps this file free of the log_* helpers.
#
# Probe order is significant and is mirrored by the Rust implementation:
#   1. not Linux  2. cross-arch  3. no nix on PATH  4. no /nix/store  5. in a container
fs_native_blocker() {
    local target_arch host_arch
    target_arch="$(fs_norm_arch "${1:-$(uname -m)}")"
    host_arch="$(fs_host_arch)"

    if [[ "$(uname -s)" != "Linux" ]]; then
        echo "host is not Linux (uname -s = $(uname -s)); image derivations are gated behind isLinux"
        return 0
    fi
    if [[ -n "$target_arch" && "$target_arch" != "$host_arch" ]]; then
        echo "cross-arch target ($target_arch) differs from host arch ($host_arch)"
        return 0
    fi
    if ! command -v nix >/dev/null 2>&1; then
        echo "nix not found on PATH"
        return 0
    fi
    if [[ ! -d "$(fs_probe_root)/nix/store" ]]; then
        echo "/nix/store does not exist on this host"
        return 0
    fi
    if fs_in_container; then
        echo "running inside a container"
        return 0
    fi

    echo ""
    return 0
}

# ── Strategy Selection ───────────────────────────────────────────────
# Echoes "native" or "docker".
fs_choose_strategy() {
    # Escape hatch FIRST, before any probing. This is a total rollback.
    case "${FIRESTREAM_BUILD_STRATEGY:-auto}" in
        native) echo "native"; return 0 ;;
        docker) echo "docker"; return 0 ;;
        auto|"") : ;;
        *)
            log_warn "Unknown FIRESTREAM_BUILD_STRATEGY='${FIRESTREAM_BUILD_STRATEGY}' (expected auto|native|docker); treating as auto"
            ;;
    esac

    if [[ -z "$(fs_native_blocker "${1:-}")" ]]; then
        echo "native"
    else
        echo "docker"
    fi
}

# ── Native build primitive ───────────────────────────────────────────
# fs_nix_build_native <flake_dir> <flake_ref> <dest> [--dir]
#
# Builds with the HOST's /nix/store and points <dest> at the result via
# --out-link. That means:
#   * zero bytes copied (dest is a symlink into the store), and
#   * dest is registered as a GC root, so `nix store gc` cannot reap a freshly
#     built image before `docker load` runs.
# Downstream is unchanged: [[ -s ]], `docker load <`, `ls -L`, and `tar -tf`
# all follow symlinks.
fs_nix_build_native() {
    local flake_dir="$1" flake_ref="$2" dest="$3"
    shift 3

    # Remaining flags (--dir / --sock) are accepted for signature parity with
    # fs_nix_build_docker; neither changes the native code path.
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dir|--sock) shift ;;
            *) shift ;;
        esac
    done

    # Untrusted users silently lose flake-supplied extra-substituters and end up
    # rebuilding the world from source. Warn rather than fail; best-effort probe.
    local _trusted
    _trusted="$(nix config show trusted-users 2>/dev/null \
                || nix show-config --extra-experimental-features "nix-command flakes" 2>/dev/null \
                   | sed -n 's/^trusted-users = //p')"
    if [[ -n "$_trusted" ]] && [[ " $_trusted " != *" ${USER:-__nouser__} "* ]] \
       && [[ " $_trusted " != *" * "* ]] && [[ "${USER:-}" != "root" ]]; then
        log_warn "'${USER:-?}' is not in nix trusted-users; flake-supplied substituters will be ignored (expect source rebuilds)"
    fi

    log_step "Strategy: native (host /nix/store)"
    log_step "Flake: $flake_ref (dir: $flake_dir)"
    log_step "Out-link: $dest"

    # `nix build --out-link` refuses to clobber a non-symlink. Past Docker
    # builds left root-owned regular files/dirs here; clear them first.
    rm -rf "$dest" 2>/dev/null || {
        log_error "Cannot remove stale build artifact: $dest"
        log_error "It may be root-owned from a previous Docker build. Try: sudo rm -rf $dest"
        return 1
    }

    ( cd "$flake_dir" && nix build "$flake_ref" \
        --out-link "$dest" \
        -L \
        --no-update-lock-file \
        --extra-experimental-features "nix-command flakes" )
}

# ── Docker build primitive ───────────────────────────────────────────
# fs_nix_build_docker <flake_dir> <flake_ref> <dest> <target_arch> [--dir] [--sock]
#
# Lifted from the previous bodies of container-images.sh / manifest.sh /
# docker-build.nix: a per-arch persistent Nix store volume, git-worktree dir
# mounts, and a `cp -L` of the dereferenced result into the mounted output dir.
#
#   --dir   <dest> is a directory of files (manifest/SBOM) rather than a tarball
#   --sock  bind-mount /var/run/docker.sock into the builder
fs_nix_build_docker() {
    local flake_dir="$1" flake_ref="$2" dest="$3" target_arch="${4:-$(uname -m)}"
    shift 4

    local want_dir=0 want_sock=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dir)  want_dir=1;  shift ;;
            --sock) want_sock=1; shift ;;
            *)      shift ;;
        esac
    done

    if ! command -v docker >/dev/null 2>&1; then
        log_error "Docker is required for the 'docker' build strategy but was not found"
        return 1
    fi

    # Resource probes are lazy - only the docker path needs them.
    if declare -F fs_docker_resources >/dev/null; then
        fs_docker_resources
    fi

    local builder_tag="nixos/nix:latest"
    local docker_platform
    docker_platform="$(fs_docker_platform "$target_arch")"

    if ! docker image inspect "$builder_tag" >/dev/null 2>&1; then
        log_info "Pulling builder image ($docker_platform)..."
        docker pull --platform "$docker_platform" "$builder_tag" >/dev/null || {
            log_error "Failed to pull $builder_tag"
            return 1
        }
    fi

    local nix_volume
    nix_volume="$(get_nix_volume "$target_arch")"

    # Host-side output dir -> /out inside the builder.
    local dest_parent dest_base
    dest_parent="$(cd "$(dirname "$dest")" && pwd -P)"
    dest_base="$(basename "$dest")"

    local docker_args=(
        --rm
        --platform "$docker_platform"
        -v "$dest_parent:/out"
        --mount "type=volume,source=$nix_volume,target=/nix"
    )

    if [[ -n "${DOCKER_CPUS:-}"   ]]; then docker_args+=(--cpus "$DOCKER_CPUS"); fi
    if [[ -n "${DOCKER_MEMORY:-}" ]]; then docker_args+=(--memory "$DOCKER_MEMORY"); fi
    if [[ -n "${DOCKER_SWAP:-}"   ]]; then docker_args+=(--memory-swap "$DOCKER_SWAP"); fi

    if [[ $want_sock -eq 1 ]]; then
        docker_args+=(-v /var/run/docker.sock:/var/run/docker.sock)
    fi

    # Mount the flake. A /nix/store snapshot (external consumer, no checkout)
    # goes to /flake; a live working tree keeps its original path so that
    # worktree .git pointers resolve.
    local workdir container_ref="$flake_ref"
    case "$flake_dir" in
        /nix/store/*)
            docker_args+=(-v "$flake_dir:/flake:ro")
            workdir="/flake"
            # Rewrite a repo-relative ref to the mounted path.
            case "$flake_ref" in
                .#*) container_ref="/flake#${flake_ref#.#}" ;;
            esac
            ;;
        *)
            flake_dir="$(cd "$flake_dir" && pwd -P)"
            docker_args+=(-v "$flake_dir:$flake_dir:ro")
            workdir="$flake_dir"
            resolve_git_mounts "$flake_dir"
            if [[ ${#GIT_DOCKER_MOUNTS[@]} -gt 0 ]]; then
                docker_args+=("${GIT_DOCKER_MOUNTS[@]}")
            fi
            ;;
    esac

    log_step "Strategy: docker (nixos/nix builder)"
    log_step "Platform: $docker_platform"
    log_step "Nix store volume: $nix_volume (persistent cache)"
    log_step "Resources: ${DOCKER_CPUS:-default} CPUs, ${DOCKER_MEMORY:-default} RAM"
    log_step "Flake: $container_ref (dir: $flake_dir)"
    if [[ "${GIT_WORKTREE_DETECTED:-false}" == "true" ]]; then
        log_step "Worktree detected - mounting git dirs at original paths"
    fi

    if [[ $want_dir -eq 1 ]]; then
        # Directory output. Copy to a container-local temp first: Nix store
        # output is read-only and chmod fails on macOS Docker volume mounts,
        # so fix perms locally before copying onto the bind mount.
        docker run "${docker_args[@]}" -w "$workdir" "$builder_tag" \
            sh -c '
                set -eu

                echo "experimental-features = nix-command flakes" >> /etc/nix/nix.conf
                git config --global --add safe.directory "*" 2>/dev/null || true

                echo ">>> Building '"$container_ref"'..."
                nix build "'"$container_ref"'" -o /tmp/result -L --no-update-lock-file

                rm -rf "/out/'"$dest_base"'"
                mkdir -p "/out/'"$dest_base"'"
                cp -rL /tmp/result /tmp/result-writable
                chmod -R u+w /tmp/result-writable
                cp -r /tmp/result-writable/* "/out/'"$dest_base"'/"
                rm -rf /tmp/result-writable

                echo ">>> Build successful"
            '
    else
        docker run "${docker_args[@]}" -w "$workdir" "$builder_tag" \
            sh -c '
                set -eu

                echo "experimental-features = nix-command flakes" >> /etc/nix/nix.conf
                git config --global --add safe.directory "*" 2>/dev/null || true

                echo ">>> Building '"$container_ref"'..."
                nix build "'"$container_ref"'" -o /tmp/result -L --no-update-lock-file

                # Dereference symlink (-L) to copy actual file, not symlink
                cp -L /tmp/result "/out/'"$dest_base"'"

                if [ ! -s "/out/'"$dest_base"'" ]; then
                    echo "ERROR: Output file empty or missing" >&2
                    exit 1
                fi

                echo ">>> Build successful"
            '
    fi
}

# ── Dispatcher ───────────────────────────────────────────────────────
# fs_build_image <flake_dir> <flake_ref> <dest> <target_arch> [--dir] [--sock]
fs_build_image() {
    local flake_dir="$1" flake_ref="$2" dest="$3" target_arch="${4:-$(uname -m)}"
    shift 4

    local strategy blocker
    strategy="$(fs_choose_strategy "$target_arch")"

    if [[ "$strategy" == "native" ]]; then
        fs_nix_build_native "$flake_dir" "$flake_ref" "$dest" "$@"
    else
        blocker="$(fs_native_blocker "$target_arch")"
        if [[ -n "$blocker" ]]; then
            log_info "Falling back to the Docker builder: $blocker"
        fi
        fs_nix_build_docker "$flake_dir" "$flake_ref" "$dest" "$target_arch" "$@"
    fi
}
