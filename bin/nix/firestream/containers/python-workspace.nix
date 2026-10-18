# Python Workspace Container Factory
# Copyright Firestream. MIT License.
#
# This module combines uv2nix workspace loading with mkPythonContainerModule.
# It allows building Python containers from a workspace directory containing
# pyproject.toml and uv.lock without needing a separate flake.nix per container.
#
# Usage:
#   let
#     container = mkPythonWorkspaceContainer {
#       workspacePath = ./src/containers/firestream/airflow;
#       name = "airflow";
#       version = "3.0.3";
#       python = pkgs.python312;
#       overrides = import ./overrides.nix { inherit pkgs lib; };
#     };
#   in container.dockerImage
#
# Consumer-owned dependencies for the PRIMARY venv (`pythonWorkspace`):
#   - replace = { src; overrides; }   consumer's pyproject.toml + uv.lock become
#                                     the primary workspace root (module.nix is
#                                     still loaded from `workspacePath`).
#   - extend  = [ { src; overrides; } ] extra uv2nix workspaces merged into the
#                                     ONE primary venv, guarded by a lock diff.
# Separate venvs (`extraWorkspaces`) remain the tool for out-of-process guests.

{ pkgs
, lib
, mkPythonContainerModule
, firestreamLib
, uv2nix
, pyproject-nix
, pyproject-build-systems
}:

let
  wsLib = import ./python-workspace-lib.nix { inherit lib; };
  noop = _final: _prev: { };
in
{
  # Factory function for Python workspace containers
  mkPythonWorkspaceContainer = {
    # Required: path to directory with pyproject.toml/uv.lock/module.nix
    workspacePath,

    # Required: container name
    name,

    # Required: container version
    version,

    # Optional: Python interpreter (default: python312)
    python ? pkgs.python312,

    # Optional: wheel and source overrides
    # Expected structure: { wheelOverrides = final: prev: { ... }; sourceOverrides = final: prev: { ... }; }
    overrides ? {},

    # Optional: additional arguments to pass to module.nix
    # These are merged directly into the module call
    moduleArgs ? {},

    # Optional: additional, separate uv2nix venvs to build alongside the
    # primary workspace (e.g. guest Airflow DAG deps). Each entry:
    #   { name; src; overrides ? {}; python ? <primary python>; }
    # Each becomes its own baked venv at /opt/firestream/${name}/${w.name}-venv.
    extraWorkspaces ? [],

    # Optional: consumer-owned dependencies for the PRIMARY venv. Shape
    # (already resolved to attrsets by eval-container.nix):
    #   { replace = null | { src; overrides; }; extend = [ { src; overrides; } ]; }
    # `null` (default) leaves the primary build byte-identical.
    pythonWorkspace ? null,

    # All other arguments are passed through to moduleArgs
    ...
  }@args:
  let
    replace = if pythonWorkspace == null then null else (pythonWorkspace.replace or null);
    extensions = if pythonWorkspace == null then [ ] else (pythonWorkspace.extend or [ ]);
    primaryRoot = if replace != null then replace.src else workspacePath;

    # ── requires-python guardrail (loud) ──────────────────────────────
    # Fail fast with a legible eval error if the effective interpreter does
    # not satisfy the workspace's declared project.requires-python, instead
    # of surfacing later as a cryptic wheel-tag / silently-mismatched venv.
    mkPythonGuard = root: python:
      let
        requiresPython =
          (builtins.fromTOML (builtins.readFile (root + "/pyproject.toml")))
            .project.requires-python or null;
      in
      if requiresPython == null then null
      else let
        conds = pyproject-nix.lib.pep440.parseVersionConds requiresPython;
        ver = pyproject-nix.lib.pep440.parseVersion python.version;
        ok = builtins.all
          (c: pyproject-nix.lib.pep440.comparators.${c.op} ver c.version)
          conds;
      in if ok then null
         else throw ''
           python-workspace: interpreter python-${python.version} does not satisfy requires-python "${requiresPython}" declared in ${toString root}/pyproject.toml'';

    loadLock = root: builtins.fromTOML (builtins.readFile (root + "/uv.lock"));

    # ── Workspace builder ─────────────────────────────────────────────
    # Loads a uv2nix workspace and resolves it TWICE:
    #   - wheel overlay  → runtime `pythonEnv` (mkVirtualEnv)
    #   - sdist overlay  → `pythonSourcePkgs` for license-compliance source
    #                      archiving / SBOM (mkVirtualEnv exposes no per-pkg .src)
    # It builds its OWN `pythonBase` so each workspace honors its own interpreter.
    #
    # `extensions` merges further workspaces into the SAME venv. With
    # `extensions == []` the derivations equal a single-workspace build.
    buildWorkspace = { workspacePath, overrides ? {}, python, envName, extensions ? [ ] }:
    let
      # Load workspace from container's uv.lock
      workspace = uv2nix.lib.workspace.loadWorkspace {
        workspaceRoot = workspacePath;
      };

      pythonGuard = mkPythonGuard workspacePath python;

      # Create overlay preferring binary wheels
      overlay = workspace.mkPyprojectOverlay {
        sourcePreference = "wheel";
      };

      # Get container-specific overrides (default to empty)
      wheelOverrides = overrides.wheelOverrides or noop;
      sourceOverrides = overrides.sourceOverrides or noop;

      # Create base Python package set (per-workspace, honors its own interpreter)
      pythonBase = pkgs.callPackage pyproject-nix.build.packages {
        inherit python;
      };

      # ── Extensions (pythonWorkspace.extend) ─────────────────────────
      extLoaded = map (e: {
        inherit (e) src;
        label = toString e.src;
        overrides = e.overrides or { };
        workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = e.src; };
        pythonGuard = mkPythonGuard e.src python;
      }) extensions;

      baseLock = loadLock workspacePath;
      extLocks = map (e: e // { lock = loadLock e.src; }) extLoaded;

      # Base-vs-extension, then extension-vs-extension pairwise.
      lockGuards =
        (map (e: wsLib.assertCompatibleLocks {
          baseLabel = toString workspacePath;
          inherit baseLock;
          extLabel = e.label;
          extLock = e.lock;
        }) extLocks)
        ++ lib.concatMap (i:
          map (j: wsLib.assertCompatibleLocks {
            baseLabel = (lib.elemAt extLocks i).label;
            baseLock = (lib.elemAt extLocks i).lock;
            extLabel = (lib.elemAt extLocks j).label;
            extLock = (lib.elemAt extLocks j).lock;
          }) (lib.range (i + 1) (lib.length extLocks - 1))
        ) (lib.range 0 (lib.length extLocks - 2));

      # deepSeq: a plain seq on the list would force only the spine, not the
      # per-extension guards inside it.
      allGuards = builtins.seq pythonGuard
        (builtins.deepSeq (map (e: e.pythonGuard) extLoaded)
          (builtins.all (g: g) lockGuards));

      extPkgOverlays = pref: map (e: e.workspace.mkPyprojectOverlay { sourcePreference = pref; }) extLoaded;
      extSourceOverrides = map (e: e.overrides.sourceOverrides or noop) extLoaded;
      extWheelOverrides = map (e: e.overrides.wheelOverrides or noop) extLoaded;

      # Compose all overlays - ORDER MATTERS
      # 1. Build systems FIRST (provides build backends like setuptools, hatchling, etc.)
      # 2. Extension package overlays, then the BASE workspace overlay LAST.
      #    NOTE: a package's build config comes from the overlay that defines
      #    it. Base-last keeps Firestream's for every shared name; the lock
      #    guard has already proven shared names share versions, so extensions
      #    only ever contribute NEW packages.
      # 3. Source build overrides third (fixes for packages built from source)
      # 4. Wheel runtime overrides last (runtime library dependencies)
      pythonSet = pythonBase.overrideScope (
        lib.composeManyExtensions (
          [ pyproject-build-systems.overlays.default ]
          ++ extPkgOverlays "wheel"
          ++ [ overlay sourceOverrides ]
          ++ extSourceOverrides
          ++ [ wheelOverrides ]
          ++ extWheelOverrides
        )
      );

      # deps.default is keyed by workspace MEMBER name; the union of the maps
      # is the root set of the single merged venv.
      depsMap = lib.foldl' (acc: e: acc // e.workspace.deps.default) workspace.deps.default extLoaded;

      # Create virtual environment with all dependencies.
      # `seq` on the guards forces the requires-python and lock checks whenever
      # the runtime env is realised, without altering the derivation (drvPath unchanged).
      pythonEnv = builtins.seq allGuards
        (pythonSet.mkVirtualEnv envName depsMap);

      # ── Source archiving support ────────────────────────────────────
      # Parallel resolution with sdist preference for source code archiving.
      # Only used for .src introspection — the runtime container uses wheel-based pythonEnv.
      sdistOverlay = workspace.mkPyprojectOverlay {
        sourcePreference = "sdist";
      };

      pythonSetSdist = pythonBase.overrideScope (
        lib.composeManyExtensions (
          [ pyproject-build-systems.overlays.default ]
          ++ extPkgOverlays "sdist"
          ++ [ sdistOverlay sourceOverrides ]
          ++ extSourceOverrides
        )
      );

      # Extract source-resolved packages for fleet-level source archiving.
      # depsMap has the member names; look each up in the sdist set.
      pythonSourcePkgs = let
        depNames = builtins.attrNames depsMap;
      in lib.filter (p: p != null) (map (name:
        let pkg = builtins.tryEval (
          if pythonSetSdist ? ${name} then pythonSetSdist.${name} else null
        );
        in if pkg.success then pkg.value else null
      ) depNames);
    in {
      inherit pythonEnv pythonSet workspace pythonSourcePkgs;
    };

    # Build the PRIMARY workspace. Outputs are byte-identical to the pre-factory
    # inline form when extraWorkspaces == [] and pythonWorkspace == null.
    primary = buildWorkspace {
      workspacePath = primaryRoot;
      inherit overrides python extensions;
      envName = "${name}-env";
    };
    inherit (primary) pythonEnv pythonSet workspace pythonSourcePkgs;

    # Build each additional workspace as its own baked venv, mounted as a sibling
    # of the primary venv at /opt/firestream/${name}/${w.name}-venv.
    extraBuilt = map (w:
      (buildWorkspace {
        workspacePath = w.src;
        overrides = w.overrides or {};
        python = w.python or python;
        envName = "${w.name}-env";
      }) // {
        inherit (w) name;
        mountPath = "/opt/firestream/${name}/${w.name}-venv";
      }
    ) extraWorkspaces;

    # Filter out factory-specific args, keep rest for moduleArgs passthrough
    extraArgs = builtins.removeAttrs args [
      "workspacePath" "name" "version" "python" "overrides" "moduleArgs"
      "extraWorkspaces" "pythonWorkspace"
    ];

    # Guest venvs for module.nix to materialize. Passed ONLY when there is at
    # least one extra workspace: module.nix has no `...` catch-all, so an
    # unconditional key would break every container whose module.nix predates
    # the `extraPythonEnvs ? []` arg — and would perturb the primary closure.
    extraPythonEnvs = map (b: { inherit (b) name pythonEnv mountPath; }) extraBuilt;

    # Build base module arguments
    baseModuleArgs = {
      inherit pkgs lib pythonEnv python;
      firestream = firestreamLib;
      # Pass version with container-specific name (airflowVersion, odooVersion, etc.)
      "${name}Version" = version;
    } // lib.optionalAttrs (extraBuilt != []) {
      inherit extraPythonEnvs;
    };

    # Merge all module arguments:
    # 1. Base args (pkgs, lib, pythonEnv, etc.)
    # 2. Extra args passed to factory (e.g., odooSource)
    # 3. Explicit moduleArgs
    allModuleArgs = baseModuleArgs // extraArgs // moduleArgs;

    # Import container module with computed arguments. module.nix ALWAYS comes
    # from Firestream's workspacePath, never from a `replace` root.
    module = import (workspacePath + "/module.nix") allModuleArgs;

  in module // {
    # Add additional exports for debugging/introspection
    inherit pythonEnv pythonSet workspace pythonSourcePkgs;

    # Append Python source packages to the container's packageList
    # so the fleet manifest can introspect their .src attributes.
    # Extra-workspace source pkgs are folded in for SBOM completeness; the
    # manifest layer dedups by store path, so no pname dedup is needed here.
    packageList = (module.packageList or [])
      ++ pythonSourcePkgs
      ++ lib.concatMap (b: b.pythonSourcePkgs) extraBuilt;

    config = (module.config or {}) // {
      inherit overrides;
    };
  };
}
