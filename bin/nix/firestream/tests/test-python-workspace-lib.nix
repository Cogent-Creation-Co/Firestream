# Tests for the pythonWorkspace helpers (containers/python-workspace-lib.nix)
# Copyright Firestream. MIT License.
#
# Pure-eval tests: every assertion is decided in Nix, and the derivation only
# materialises the verdict. The inline TOML strings stand in for uv.lock files.
{ pkgs, firestream }:

let
  lib = pkgs.lib;
  w = firestream.pythonWorkspaceLib;

  lock = s: builtins.fromTOML s;

  base = lock ''
    version = 1

    [[package]]
    name = "firestream-odoo"
    version = "18.0.0"
    source = { editable = "." }

    [[package]]
    name = "plaid-python"
    version = "43.0.0"
    source = { registry = "https://pypi.org/simple" }

    [[package]]
    name = "requests"
    version = "2.32.0"
    source = { registry = "https://pypi.org/simple" }
  '';

  compatible = lock ''
    version = 1

    [[package]]
    name = "extra-deps"
    version = "0.1.0"
    source = { editable = "." }

    [[package]]
    name = "plaid-python"
    version = "43.0.0"
    source = { registry = "https://pypi.org/simple" }

    [[package]]
    name = "pycairo"
    version = "1.29.1"
    source = { registry = "https://pypi.org/simple" }
  '';

  versionConflict = lock ''
    version = 1

    [[package]]
    name = "extra-deps"
    version = "0.1.0"
    source = { editable = "." }

    [[package]]
    name = "plaid-python"
    version = "44.0.0"
    source = { registry = "https://pypi.org/simple" }
  '';

  memberCollision = lock ''
    version = 1

    [[package]]
    name = "firestream-odoo"
    version = "0.1.0"
    source = { virtual = "." }
  '';

  # uv.lock can hold two entries for one name under fork markers.
  forkedBase = lock ''
    version = 1

    [[package]]
    name = "numpy"
    version = "1.26.4"
    source = { registry = "https://pypi.org/simple" }

    [[package]]
    name = "numpy"
    version = "2.2.0"
    source = { registry = "https://pypi.org/simple" }
  '';

  forkedSame = lock ''
    version = 1

    [[package]]
    name = "numpy"
    version = "2.2.0"
    source = { registry = "https://pypi.org/simple" }

    [[package]]
    name = "numpy"
    version = "1.26.4"
    source = { registry = "https://pypi.org/simple" }
  '';

  forkedDiff = lock ''
    version = 1

    [[package]]
    name = "numpy"
    version = "2.2.0"
    source = { registry = "https://pypi.org/simple" }
  '';

  check = { baseLock, extLock }: builtins.tryEval
    (builtins.deepSeq
      (w.assertCompatibleLocks { baseLabel = "base"; inherit baseLock; extLabel = "ext"; inherit extLock; })
      true);

  # tryEval does not expose the thrown message, so the message text is
  # asserted through lockConflicts, which is what the throw renders.
  conflicts = w.lockConflicts { baseLock = base; extLock = versionConflict; };

  # composeOverrides: `b` must see `a`'s result in prev, for both keys.
  a = { wheelOverrides = _f: _p: { x = 1; }; sourceOverrides = _f: _p: { s = "a"; }; };
  b = { wheelOverrides = _f: p: { x = p.x + 1; }; };
  composed = w.composeOverrides a b;
  composedWheel = lib.fix (lib.extends composed.wheelOverrides (_: { }));
  composedSource = lib.fix (lib.extends composed.sourceOverrides (_: { }));

  results = {
    compatiblePasses = (check { baseLock = base; extLock = compatible; }).success;
    versionConflictThrows = !(check { baseLock = base; extLock = versionConflict; }).success;
    memberCollisionThrows = !(check { baseLock = base; extLock = memberCollision; }).success;
    conflictNamesPackage = conflicts.versions == [ { name = "plaid-python"; base = [ "43.0.0" ]; ext = [ "44.0.0" ]; } ];
    conflictListsNoMembers = conflicts.members == [ ];
    memberCollisionNamed = (w.lockConflicts { baseLock = base; extLock = memberCollision; }).members == [ "firestream-odoo" ];
    forkedSameOrderInsensitive = (check { baseLock = forkedBase; extLock = forkedSame; }).success;
    forkedSubsetThrows = !(check { baseLock = forkedBase; extLock = forkedDiff; }).success;
    membersExcludedFromVersions = !(w.versionsByName base ? firestream-odoo);
    localMembersFound = w.localMemberNames base == [ "firestream-odoo" ];
    composeAppliesBoth = composedWheel.x == 2;
    composeKeepsMissingKeyNoop = composedSource.s == "a";
  };

  failed = lib.filterAttrs (_: ok: !ok) results;
in
pkgs.runCommand "test-python-workspace-lib" { } (
  if failed == { } then ''
    echo "PASS: python-workspace-lib (${toString (builtins.length (builtins.attrNames results))} assertions)"
    touch $out
  '' else ''
    echo "FAIL: python-workspace-lib assertions failed: ${lib.concatStringsSep ", " (builtins.attrNames failed)}"
    exit 1
  ''
)
