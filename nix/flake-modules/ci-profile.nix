# CI profile flake-module
# Copyright Firestream. MIT License.
#
# Emits `packages.firestream-ci-profile` — a store directory containing
# `ci-manifest.json`, the runtime payload `firestream_ci::profile` reads.
#
# This is the CI-profile mirror of nix/flake-modules/charts/aggregate.nix:
#
#   packages.firestream-charts-bundle  ->  index.json + per-chart manifests
#                                          -> FIRESTREAM_CHARTS_DIR
#   packages.firestream-ci-profile     ->  ci-manifest.json
#                                          -> FIRESTREAM_CI_PROFILE
#
# ── Why per-system, and why that matters ─────────────────────────────────────
# `perSystem` evaluates this once per system, and eval-ci.nix bakes
# `project.nix_system` / `project.arch` into each manifest. That is the ENTIRE
# arch-gating mechanism: a target that exists on only one system is added by an
# `lib.optionals` on the Nix side, and `firestream_ci` never branches on arch.
# The reference implementation this replaces did it with `if arch == "x86_64"`
# inside Rust; see src/util/firestream-ci/docs/defaults-reference.rs.txt and the
# `build_attrs_aarch64_excludes_gpu` test in tests/profile_fixtures.rs.
#
# ── Darwin ───────────────────────────────────────────────────────────────────
# nix/flake-modules/checks.nix is Linux-gated (the test suite pulls Linux-only
# closures) and container images are Linux-only. The Darwin manifest is
# therefore emitted with the verify/build phases EMPTY rather than listing
# attributes that do not exist there — a profile that names a nonexistent attr
# would fail at build time, far from the cause.
#
# The tidy phase's builtin tasks are cleared too, so the Darwin manifest has
# ZERO runnable phases. That is deliberate: `firestream-ci ci-linux` treats an
# empty runnable-phase set as a hard error ("this profile declares no runnable
# phases for <system>/<mode>"), which is what makes a Darwin `ci-linux` an
# explicit "not supported here" rather than a run that does nothing and exits
# green. `ci-darwin` — Linux images built through Docker from a macOS host — is
# the supported Darwin path and is a separate runner.
{ inputs, ... }: {
  perSystem = { pkgs, lib, system, config, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      evalCi = import ../../bin/nix/firestream/ci/eval-ci.nix { inherit pkgs lib; };

      # Blank out the attr lists on non-Linux. `mkForce` so it wins over the
      # profile module's own values rather than merging with them.
      darwinTrim = { lib, ... }: {
        config.ci.phases = {
          tidy.builtinTasks = lib.mkForce [ ];
          verify.attrs = lib.mkForce [ ];
          build.attrs = lib.mkForce [ ];
          attest.attrs = lib.mkForce [ ];
        };
      };

      result = evalCi {
        nixSystem = system;
        modules = [ ../../bin/nix/firestream/ci/profile.nix ]
          ++ lib.optional (!isLinux) darwinTrim;
        # The profile DERIVES its verify list from these rather than carrying a
        # hand-maintained copy, so a check added to the flake is gated by CI the
        # same day. On Darwin `config.checks` is empty (checks.nix is
        # Linux-gated), which is what keeps the Darwin manifest at zero runnable
        # phases — see the header.
        checkNames = builtins.attrNames config.checks;
        provenance = {
          flakeRevision = inputs.self.rev or inputs.self.dirtyRev or "dev";
        };
      };
    in
    {
      packages.firestream-ci-profile = result.profileDir;
    };
}
