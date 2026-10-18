# CI profile shared type: pipeline phase
# Copyright Firestream. MIT License.
#
# One phase of the CI DAG. Execution order is the topological order of
# `dependsOn`, not declaration order — the Rust reader
# (`firestream_ci::profile::Profile::phase_order`) runs Kahn's algorithm with
# ties broken by declaration order, so the emitted list stays deterministic.
#
# `attrs` / `advisoryAttrs` are TEMPLATES over `{system}` / `{arch}` /
# `{project}`. That is the entire templating vocabulary, and it is load-bearing:
# arch-conditional attribute sets (the reference's x86_64-only GPU package) are
# expressed by emitting a system-specific manifest, NOT by any branch on the
# consumer side. If a phase's attr list must differ per system, this file is not
# where that happens — the flake-module that instantiates the profile is.
#
# TWO tier mechanisms, both needed, both present in the reference:
#   * `attrs`         — tier comes from the top-level `tierRules` / `tierDefault`
#                       classification. Suits a project that encodes tier in its
#                       attribute names (`required-*` / `advisory-*`), where a
#                       newly added `advisory-` check is advisory the moment it
#                       exists.
#   * `advisoryAttrs` — forced Advisory regardless of `tierRules`. Suits a
#                       project whose check names carry no tier convention at
#                       all — which is Firestream: every check in
#                       nix/flake-modules/checks.nix is `firestream-*`.

{ lib, ... }:

let
  inherit (lib) mkOption types;
  matcher = import ./matcher.nix { inherit lib; };

in {
  phaseType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        description = "Phase name. Defaults to the attribute key.";
        example = "verify";
      };

      tier = mkOption {
        type = matcher.tierType;
        default = "required";
        description = ''
          Phase-level tier. A failing `required` phase fails the run (exit 1)
          and skips downstream phases; a failing `advisory` phase downgrades
          the verdict to PartiallyPassed (exit 2) and blocks nothing.
        '';
      };

      dependsOn = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Phases that must complete before this one.";
        example = [ "verify" ];
      };

      order = mkOption {
        type = types.int;
        default = 100;
        description = ''
          Sort key used ONLY to make the emitted `phases` array deterministic
          (Nix attrsets are unordered). It does not affect execution order,
          which comes from `dependsOn`.
        '';
      };

      modes = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          CI modes this phase runs in (`check`, `release`). Empty list means
          every mode — that is the common case, so it is the default.
        '';
        example = [ "release" ];
      };

      attrs = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Flake attribute templates built in this phase. `{system}`, `{arch}`
          and `{project}` are expanded by the consumer against the RUNTIME
          system/arch (the CLI's `--nix-system` / `--arch` win over the values
          baked into `project`).
        '';
        example = [ "checks.{system}.firestream-tests" ];
      };

      advisoryAttrs = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Like `attrs`, but forced to the `advisory` tier regardless of what
          `tierRules` would classify them as. Failures are reported and never
          gate.
        '';
      };

      builtinTasks = mkOption {
        type = types.listOf (types.enum [ "nix-gc" "target-sweep" ]);
        default = [ ];
        description = ''
          Runner-provided tasks that are NOT nix attributes. The vocabulary is
          closed and belongs to the tool, not to any project:

            * `nix-gc`       - `nix-collect-garbage --delete-older-than 7d`.
                               Refuses to touch a HOST store unless the runner
                               is inside the docker builder; see the guard in
                               `firestream_ci`'s tidy task.
            * `target-sweep` - repo-local `target/` + `_build/` hygiene. Host
                               safe by construction (it never touches
                               /nix/store), so it runs in every mode.

          Declared here rather than keyed off the phase NAME in Rust, so a
          project that calls its hygiene phase something else still works.
        '';
        example = [ "nix-gc" "target-sweep" ];
      };

      shellTasks = mkOption {
        type = types.listOf (types.submodule {
          options = {
            name = mkOption {
              type = types.str;
              description = "Task name in the dashboard, summary and log file names.";
            };
            command = mkOption {
              type = types.listOf types.str;
              description = ''
                Full argv. `command` is exec'd directly - nothing is passed
                through a shell, so no quoting rules apply. `{system}` /
                `{arch}` / `{project}` are expanded in each element.
              '';
              example = [ "bash" "./bin/test/e2e.sh" ];
            };
          };
        });
        default = [ ];
        description = ''
          Free-form command tasks. This is the seam for non-hermetic steps that
          cannot be a flake attr - an e2e harness that binds a socket, a deploy
          smoke test. Keeping it as profile data is what stops such a step from
          being hardcoded into the runner by phase name.
        '';
      };

      aggregate = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Collapse this phase's `attrs` into ONE `nix-fast-build` invocation
          over their common attribute-path prefix, instead of one invocation
          per attr.

          This is the difference between N concurrent multi-GB flake
          evaluations fighting over the store lock and a single evaluation
          feeding one bounded job queue. Per-attr verdicts, spans and GC roots
          are unchanged - the result file still carries one entry per
          attribute.

          Requires the phase's attrs to share a dotted prefix and
          `advisoryAttrs` to be empty (an aggregate task has exactly one tier:
          the phase's). The runner falls back to per-attr invocations with a
          warning when either does not hold.
        '';
      };

      exportArtifacts = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Materialise this phase's build outputs into `<rundir>/artifacts/`
          (with sha256 + size) through the top-level `exportTargets` rules, and
          record a manifest entry for each. Off by default - a `verify` phase's
          check derivations are not release artifacts.
        '';
      };
    };
  });
}
