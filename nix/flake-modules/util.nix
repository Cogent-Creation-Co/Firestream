# `src/util/` toolkit flake-module
# Copyright Firestream. MIT License. (The packaged sources under `src/util/`
# are Apache-2.0 — see `src/util/LICENSE`; the root MIT licence does not cover
# that subtree.)
#
# Builds the three binaries that live in the ISOLATED Cargo workspace at
# `src/util/`:
#
#   packages.firestream-ci        (crate firestream-ci,        bin firestream-ci)
#   packages.otel-cli             (crate firestream-otel-cli,  bin otel-cli)
#   packages.firestream-nix-build (crate firestream-nix-build, bin firestream-nix-build)
#
# ── Why not `firestreamLib.mkRustPackage`? ────────────────────────────────────
# `mkRustPackage` (bin/nix/firestream/rust/mkRustPackage.nix) builds ONE package
# per call and derives its own `buildDepsOnly` layer per call. Pointing it at
# `src/util` three times would build the (large: tonic, axum ×2, reqwest,
# rustls/aws-lc, bollard, git2, superconsole, ratatui) dependency closure three
# times over, and it offers no seam to set `doCheck = false` (crane's
# `buildPackage` runs `cargo test` in the check phase by default; the util
# workspace's tests are covered by `make test-util`, not by `nix build`).
#
# So we use the same crane lib it uses — `firestreamLib.rust.craneLib`, i.e. the
# identical fenix stable toolchain and the same `crane` flake input — directly,
# with ONE shared `cargoArtifacts` layer feeding three `buildPackage` calls.
# Conventions (buildInputs, pkg-config/clang, LIBCLANG_PATH) mirror
# mkRustPackage.nix so the two stay recognisably the same shape.
#
# ── Source filter ─────────────────────────────────────────────────────────────
# `src` is `../../src/util`, NOT the repo root, and it is run through
# `lib.cleanSourceWith`. The filtered copy is content-addressed, so the build is
# busted only by changes under `src/util/` — an edit to a chart, a container
# module, or the root Cargo workspace does not rebuild these binaries.
#
# ── Native deps ───────────────────────────────────────────────────────────────
#   zlib      REQUIRED. git2 → libgit2-sys → libz-sys links the system libz;
#             without it nothing that pulls in `firestream_ci` can even launch
#             (`error while loading shared libraries: libz.so.1`).
#   cmake/perl  aws-lc-sys (rustls 0.23's default provider) builds via cmake.
#   protobuf  deliberately NOT here. `firestream-ci`'s `codegen` feature is off
#             by default and `src/wire/` is committed, so the default build is
#             hermetic and needs no `protoc`. `protoc` is provided in the dev
#             shell instead, for regenerating.
{ ... }: {
  perSystem = { pkgs, lib, firestreamLib, ... }:
    let
      craneLib = firestreamLib.rust.craneLib;

      utilRoot = ../../src/util;

      # DENY-list, not an allow-list. An allow-list of "cargo sources + a few
      # extensions" silently drops non-Rust build inputs: firestream-ci
      # `include_str!`s three `src/service/templates/*.template` files, and
      # `[package] readme`/licence files are named by Cargo.toml. The whole
      # subtree is ~1 MB of text, so taking all of it and subtracting build
      # scratch is both correct and cheap.
      #
      # `target/` matters: it is absent from the flake source (git-tracked only)
      # but present when building from a plain directory copy — and there are
      # TWO of them, `src/util/target` and the nested
      # `firestream-ci/tests/lift-fixture/target`.
      utilFilter = path: type:
        let base = baseNameOf (toString path);
        in
        if type == "directory"
        then !(lib.elem base [ "target" ".git" "node_modules" ])
        else !(lib.elem base [ ".DS_Store" ] || lib.hasSuffix ".rs.bk" base);

      cleanedSrc = lib.cleanSourceWith {
        name = "firestream-util-source";
        src = utilRoot;
        filter = utilFilter;
      };

      commonArgs = {
        # The source root IS the util workspace root, so crane vendors from
        # `src/util/Cargo.lock` — never the repo-root lockfile.
        src = cleanedSrc;

        nativeBuildInputs = with pkgs; [
          pkg-config
          cmake
          perl
          llvmPackages.clang
          llvmPackages.libclang.lib
        ];

        buildInputs = with pkgs; [
          zlib
          openssl
          openssl.dev
        ] ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];

        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

        # zlib in `buildInputs` is enough to LINK but not to RUN: libz-sys
        # locates zlib via pkg-config and emits a bare `-L` on rustc's command
        # line, which nixpkgs' ld-wrapper does not turn into an RPATH entry.
        # The result is a `firestream-ci` binary with `NEEDED libz.so.1` and an
        # EMPTY RPATH — it links clean and then dies at exec with
        # "error while loading shared libraries: libz.so.1". Pin it explicitly.
        # Set on `commonArgs` (not just the final build) so the shared
        # `cargoArtifacts` layer is built with the identical flag.
        RUSTFLAGS = lib.optionalString pkgs.stdenv.hostPlatform.isLinux
          "-C link-arg=-Wl,-rpath,${pkgs.zlib}/lib";

        # Tests belong to `make test-util` / `cargo test` in the dev shell; some
        # of them shell out to git/docker and are not hermetic.
        doCheck = false;
      };

      # ONE dependency layer, shared by all three binaries.
      cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
        pname = "firestream-util-deps";
        version = "0.1.0";
      });

      mkUtilBin = { pname, cratePackage, bin, description }:
        craneLib.buildPackage (commonArgs // {
          inherit pname cargoArtifacts;
          version = "0.1.0";
          cargoExtraArgs = "--locked --package ${cratePackage} --bin ${bin}";

          meta = {
            inherit description;
            homepage = "https://github.com/Cogent-Creation-Co/Firestream";
            license = lib.licenses.asl20;
            mainProgram = bin;
            platforms = lib.platforms.unix;
          };
        });
    in
    {
      packages.firestream-ci = mkUtilBin {
        pname = "firestream-ci";
        cratePackage = "firestream-ci";
        bin = "firestream-ci";
        description = "Firestream build-pipeline toolkit (Nix + Docker + OpenTelemetry)";
      };

      packages.otel-cli = mkUtilBin {
        pname = "otel-cli";
        cratePackage = "firestream-otel-cli";
        bin = "otel-cli";
        description = "OpenTelemetry CLI for emitting spans from shell scripts and CI pipelines";
      };

      packages.firestream-nix-build = mkUtilBin {
        pname = "firestream-nix-build";
        cratePackage = "firestream-nix-build";
        bin = "firestream-nix-build";
        description = "Parallel Nix builder with in-process OpenTelemetry ingest";
      };
    };
}
