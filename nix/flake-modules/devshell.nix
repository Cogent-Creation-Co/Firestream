# Dev shell flake-module
# Copyright Firestream. MIT License.
#
# Ports the legacy flake's devShells.default 1:1. On Darwin uses mkShellNoCC
# (avoids stdenv's automatic SDK setup); shellHook comes from modules/darwin.nix.
#
# Phase 5: also wires `FIRESTREAM_CHARTS_DIR` to the store path of
# `packages.firestream-charts-bundle` so the Rust CLI's `helm deploy`
# subcommand finds an index.json without operators having to set the env var
# manually. Env var only — nothing is written into the working tree.
{ inputs, ... }: {
  perSystem = { pkgs, system, self', lib, ... }:
    let
      isDarwin = pkgs.stdenv.isDarwin;

      shellEnv = import ./lib/shell-env.nix {
        inherit pkgs system;
        inherit (inputs) fenix;
      };

      darwin = import ../../bin/nix/firestream/modules/darwin.nix {
        inherit pkgs;
        lib = pkgs.lib;
      };

      # `FIRESTREAM_CHARTS_DIR` is an env var, not a file. It points straight
      # at the bundle's store path so `firestream helm deploy ...` works
      # without an explicit --charts-dir.
      #
      # This used to `nix build --out-link "$PWD/.firestream/charts"`, which
      # left a /nix/store symlink in the working tree — it got committed, it
      # dangles on every other machine, and it re-diffs on every chart rebuild.
      # Interpolating the package instead makes the bundle a devShell build
      # input, so `nix develop` (and direnv's flake profile) keeps it GC-alive:
      # the out-link's only other job is preserved, with nothing on disk.
      #
      # Cost of the swap: the bundle is no longer best-effort. A broken chart
      # build now blocks shell entry rather than degrading to a warning. If
      # that bites, the lazy variant is a drop-in — `nix build --no-link
      # --print-out-paths` in the hook, warn-and-leave-unset on failure — at
      # the price of an eval per shell entry and no GC root.
      chartsHook = ''
        export FIRESTREAM_CHARTS_DIR="${self'.packages.firestream-charts-bundle}"
      '';

      # `src/util/` toolkit (nix/flake-modules/util.nix). Put on PATH so
      # `firestream-ci --version` / `otel-cli ...` work inside `nix develop`
      # without a manual `nix run`.
      #
      # COST: unlike the best-effort chart bundle above, these are hard devShell
      # inputs — the first `nix develop` after a src/util change builds the
      # workspace (large dep closure: tonic, axum, reqwest, rustls, bollard,
      # git2, superconsole, ratatui). Rolling that back is just deleting the
      # three entries; nothing else depends on them being here.
      utilPackages = [
        self'.packages.firestream-ci
        self'.packages.otel-cli
        self'.packages.firestream-nix-build

        # `protoc`, for regenerating firestream-ci's committed `src/wire/` via
        # `cargo build -p firestream-ci --features codegen`. The default build
        # is hermetic and does NOT need this — it exists for developers only.
        pkgs.protobuf

        # RUNTIME dependencies of the pipeline, not developer conveniences.
        # firestream-nix-build spawns `nix-eval-jobs` to enumerate a flake's
        # derivations, so WITHOUT this every `verify`/`build` phase dies at
        # `spawn nix-eval-jobs: No such file or directory` before evaluating a
        # single attr — i.e. the whole pipeline is inert. Found by running
        # `make ci-e2e` for real; a dry-run cannot catch it.
        pkgs.nix-eval-jobs

        # `firestream-ci sweep`'s target/ pass shells out to `cargo sweep`.
        # Absent, it degrades to a warning and prunes only _build/ rundirs, so
        # the tidy phase silently does half its job.
        pkgs.cargo-sweep
      ];

      # cargo-built (NOT Nix-built) binaries in this repo have no RPATH, so any
      # `-sys` crate's shared library must be on the loader path or the binary
      # exits 127 before running a single test. Two workspaces need this:
      #
      #   src/util   git2 → libgit2-sys → libz-sys              (libz)
      #   root       kube/reqwest → openssl-sys                 (libssl, libcrypto)
      #              firestream-e2e-k8s → ... → bzip2-sys       (libbz2)
      #
      # libbz2 and libssl were found by running `make test-e2e-k8s-redis`, which
      # died at `libbz2.so.1: cannot open shared object file` — a dry run cannot
      # catch this, because the binary has to actually exec.
      #
      # FIRESTREAM_UTIL_LIB_PATH is kept as the name `make test-util` reads, so
      # that target still works from a bare shell.
      # LD_LIBRARY_PATH is inert on Darwin (dyld ignores it); these libs come
      # from libSystem there, so nothing extra is needed.
      utilHook = ''
        export FIRESTREAM_UTIL_LIB_PATH="${lib.makeLibraryPath [
          pkgs.zlib
          pkgs.openssl
          pkgs.bzip2
        ]}"
        export LD_LIBRARY_PATH="$FIRESTREAM_UTIL_LIB_PATH''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      '';

      # CI profile (`ci-manifest.json`, nix/flake-modules/ci-profile.nix), by
      # the same rule as `chartsHook` above: an env var pointing at a store
      # path, no out-link in the working tree.
      #
      # NOTE the resolver accepts a DIRECTORY here — it appends
      # `ci-manifest.json` — and a store path is a directory, so this points
      # at the package root exactly as FIRESTREAM_CHARTS_DIR does.
      ciProfileHook = ''
        export FIRESTREAM_CI_PROFILE="${self'.packages.firestream-ci-profile}"
      '';
    in
    {
      devShells.default = (if isDarwin then pkgs.mkShellNoCC else pkgs.mkShell) {
        packages = shellEnv.shellPackages ++ utilPackages;
        shellHook = darwin.shellHook + chartsHook + utilHook + ciProfileHook;
      };
    };
}
