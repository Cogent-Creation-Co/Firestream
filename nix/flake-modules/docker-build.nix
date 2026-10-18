# Linux image builder flake-module (native-first, Docker fallback)
# Copyright Firestream. MIT License.
#
# Makes "build a Linux container image" a first-class flake capability on every
# host. Firestream's container images are built with dockerTools and are gated
# behind `isLinux`; on macOS `nix build .#airflow` yields a stub. This module
# ships a self-contained shell app that builds against the host's /nix/store
# when that is possible, and otherwise runs the Nix build *inside* a `nixos/nix`
# Linux container (selecting `--platform`); either way it can then load the
# resulting image tarball into the local Docker daemon.
#
# The native-vs-Docker decision lives in bin/build/strategy.sh, sourced below
# as a /nix/store path so this app stays usable without a repo checkout.
#
# It contributes (on EVERY system, Darwin and Linux):
#   - apps.build-image            generic: `nix run .#build-image -- <pkg> [opts]`
#   - apps.<name>-image           one per image registry key (airflow-image, ...)
#   - _module.args.firestreamBuildImage   the builder derivation (used by compose.nix)
#
# The Docker fallback mirrors the proven bin/build/container-images.sh: a
# persistent per-arch Nix store volume for caching, git-worktree dir mounts, and
# `docker load` of the dereferenced tarball.
{ inputs, ... }: {
  perSystem = { pkgs, lib, config, system, ... }:
    let
      # Default target arch = host arch (aarch64-darwin -> aarch64 -> linux/arm64).
      hostArch = lib.head (lib.splitString "-" system);

      builder = pkgs.writeShellScriptBin "firestream-build-image" ''
        set -euo pipefail
        export PATH=${lib.makeBinPath [ pkgs.coreutils pkgs.gnused pkgs.gnugrep pkgs.gawk pkgs.git ]}:"$PATH"

        # Shared build-strategy predicate + build primitives. Baked in as a
        # store path so this works with no repo checkout. NOTE: deliberately no
        # pkgs.nix in makeBinPath above - the host's `nix` (and its daemon
        # socket) must win; a store `nix` would bloat the closure and risk a
        # client/daemon version mismatch.
        # shellcheck source=../../bin/build/strategy.sh
        source ${../../bin/build/strategy.sh}

        # Baked at app-build time: the flake's own source (store path) is the
        # fallback when not invoked from inside a Firestream working tree.
        SELF_STORE_PATH=${inputs.self}
        DEFAULT_ARCH=${lib.escapeShellArg hostArch}

        usage() {
          cat <<'EOF'
        firestream-build-image — build a Firestream container image with Nix
        (natively when possible, otherwise inside a nixos/nix Docker builder).

        Usage: firestream-build-image <package> [options]

          <package>            Flake package to build (e.g. airflow, postgresql, redis, kafka)

        Options:
          --target <arch>      Target arch: x86_64 | aarch64  (default: host arch)
          --load               Load the image into the local Docker daemon (default)
          --no-load            Build the tarball but do not load it
          --out <dir>          Directory for the output tarball (default: ./.firestream-build)
          --flake <ref>        Flake directory to build from (default: enclosing
                               Firestream repo, else the pinned firestream source)
          --native             Force a native build against the host /nix/store
          --docker             Force the nixos/nix Docker builder
          -h, --help           Show this help

        Strategy defaults to native on Linux when the target arch matches the
        host, `nix` is on PATH, and we are not inside a container; otherwise the
        Docker builder. Override globally with FIRESTREAM_BUILD_STRATEGY=auto|native|docker.
        EOF
        }

        PKG=""
        TARGET_ARCH="$DEFAULT_ARCH"
        DO_LOAD=1
        OUT_DIR=""
        FLAKE_DIR_OVERRIDE=""

        while [ $# -gt 0 ]; do
          case "$1" in
            --target|--arch) TARGET_ARCH="$2"; shift 2 ;;
            --load)          DO_LOAD=1; shift ;;
            --no-load)       DO_LOAD=0; shift ;;
            --out)           OUT_DIR="$2"; shift 2 ;;
            --flake)         FLAKE_DIR_OVERRIDE="$2"; shift 2 ;;
            --native)        export FIRESTREAM_BUILD_STRATEGY=native; shift ;;
            --docker)        export FIRESTREAM_BUILD_STRATEGY=docker; shift ;;
            -h|--help)       usage; exit 0 ;;
            -*)              echo "Unknown option: $1" >&2; usage; exit 1 ;;
            *)               if [ -z "$PKG" ]; then PKG="$1"; else echo "Unexpected argument: $1" >&2; exit 1; fi; shift ;;
          esac
        done

        [ -n "$PKG" ] || { usage; exit 1; }

        # docker is no longer an unconditional requirement - it is needed only
        # for the docker strategy (checked inside fs_nix_build_docker) or for
        # --load.
        STRATEGY="$(fs_choose_strategy "$TARGET_ARCH")"
        if [ "$STRATEGY" = "docker" ] || [ "$DO_LOAD" -eq 1 ]; then
          command -v docker >/dev/null 2>&1 || { echo "ERROR: docker not found on PATH" >&2; exit 1; }
        fi

        # Find the enclosing Firestream working tree (flake.nix + container dir).
        find_repo_root() {
          local dir
          dir="$(cd "$1" 2>/dev/null && pwd -P)" || return 1
          while [ "$dir" != "/" ]; do
            if [ -f "$dir/flake.nix" ] && [ -d "$dir/src/containers/firestream" ]; then
              echo "$dir"; return 0
            fi
            dir="$(dirname "$dir")"
          done
          return 1
        }

        if [ -n "$FLAKE_DIR_OVERRIDE" ]; then
          FLAKE_DIR="$FLAKE_DIR_OVERRIDE"
        elif FLAKE_DIR="$(find_repo_root "$PWD")"; then
          :
        else
          FLAKE_DIR="$SELF_STORE_PATH"
        fi

        OUT_DIR="''${OUT_DIR:-$PWD/.firestream-build}"
        mkdir -p "$OUT_DIR"

        # A /nix/store source snapshot is itself a valid flake directory, so the
        # native path needs no bind-mount/worktree apparatus at all: pass the
        # resolved dir plus the uniform ".#$PKG" ref. fs_nix_build_docker owns
        # the equivalent mount handling (/flake for store snapshots, original
        # path + git dirs for a live worktree) for the fallback.
        case "$FLAKE_DIR" in
          /nix/store/*) : ;;
          *)            FLAKE_DIR="$(cd "$FLAKE_DIR" && pwd -P)" ;;
        esac

        echo ">>> Building $PKG for $TARGET_ARCH (flake: $FLAKE_DIR)..." >&2
        fs_build_image "$FLAKE_DIR" ".#$PKG" "$OUT_DIR/$PKG.tar.gz" "$TARGET_ARCH"

        TARBALL="$OUT_DIR/$PKG.tar.gz"
        [ -s "$TARBALL" ] || { echo "ERROR: build produced no tarball: $TARBALL" >&2; exit 1; }
        echo ">>> Built: $TARBALL" >&2

        if [ "$DO_LOAD" -eq 1 ]; then
          echo ">>> Loading into Docker daemon..." >&2
          docker load < "$TARBALL"
        fi
      '';

      imageNames = builtins.attrNames config.firestreamImages;

      mkImageApp = name: {
        type = "app";
        program = "${pkgs.writeShellScriptBin "firestream-image-${name}" ''
          exec ${builder}/bin/firestream-build-image ${lib.escapeShellArg name} "$@"
        ''}/bin/firestream-image-${name}";
      };
    in
    {
      _module.args.firestreamBuildImage = builder;

      apps = {
        build-image = { type = "app"; program = "${builder}/bin/firestream-build-image"; };
      } // lib.listToAttrs (map (n: { name = "${n}-image"; value = mkImageApp n; }) imageNames);
    };
}
