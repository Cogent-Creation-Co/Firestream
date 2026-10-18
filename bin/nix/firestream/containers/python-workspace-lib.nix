# Pure helpers for the `pythonWorkspace` consumer seam
# Copyright Firestream. MIT License.
#
# These functions take already-parsed data (a `builtins.fromTOML`'d uv.lock,
# an overrides attrset) and return plain values or throw. They import no
# uv2nix / pyproject.nix code on purpose: the module-system test suite
# (bin/nix/firestream/tests) evaluates Firestream WITHOUT the Python packaging
# inputs, and this keeps the lock-compatibility contract unit-testable there.
#
# Overrides attrset shape (the contract every <workspace>/overrides.nix meets):
#   { wheelOverrides = final: prev: { ... }; sourceOverrides = final: prev: { ... }; }

{ lib }:

let
  noop = _final: _prev: { };

  # NOTE: uv.lock records the workspace's own members with `source.editable`
  # (a packaged member) or `source.virtual` (a virtual root). Everything else
  # in `package[]` is a resolved third-party dependency.
  isMember = p: (p.source or { }) ? editable || (p.source or { }) ? virtual;

  packagesOf = lock: lock.package or [ ];

  formatList = xs: lib.concatMapStringsSep ", " toString xs;
in
rec {
  emptyOverrides = {
    wheelOverrides = noop;
    sourceOverrides = noop;
  };

  # composeOverrides :: overrides -> overrides -> overrides
  # `b` is applied after `a` for both keys, so `b` sees `a`'s results in `prev`.
  composeOverrides = a: b: {
    wheelOverrides = lib.composeExtensions (a.wheelOverrides or noop) (b.wheelOverrides or noop);
    sourceOverrides = lib.composeExtensions (a.sourceOverrides or noop) (b.sourceOverrides or noop);
  };

  # localMemberNames :: lock -> [ str ]
  localMemberNames = lock: lib.unique (map (p: p.name) (lib.filter isMember (packagesOf lock)));

  # versionsByName :: lock -> { <name> = [ version ]; }
  # A uv.lock may carry several entries for one name under fork markers, so a
  # name maps to a sorted, unique version LIST and callers compare lists.
  versionsByName = lock:
    lib.mapAttrs (_: vs: lib.unique (lib.sort lib.lessThan vs))
      (lib.foldl'
        (acc: p: acc // { ${p.name} = (acc.${p.name} or [ ]) ++ [ (p.version or "<none>") ]; })
        { }
        (lib.filter (p: !isMember p) (packagesOf lock)));

  # lockConflicts :: { baseLock; extLock; } -> { versions = [ { name; base; ext; } ]; members = [ str ]; }
  lockConflicts = { baseLock, extLock }:
    let
      baseV = versionsByName baseLock;
      extV = versionsByName extLock;
      shared = lib.intersectLists (lib.attrNames baseV) (lib.attrNames extV);
      versions = lib.concatMap
        (n: lib.optional (baseV.${n} != extV.${n}) { name = n; base = baseV.${n}; ext = extV.${n}; })
        shared;
      members = lib.intersectLists (localMemberNames baseLock) (localMemberNames extLock);
    in
    { inherit versions members; };

  # assertCompatibleLocks :: { baseLabel; baseLock; extLabel; extLock; } -> true (or throw)
  #
  # WARNING: pyproject.nix's venv resolver performs no version validation. Two
  # locks that disagree on a shared package would otherwise resolve silently to
  # whichever overlay the factory composes last. This guard is what makes
  # `pythonWorkspace.extend` safe; do not weaken it to a warning.
  assertCompatibleLocks = { baseLabel, baseLock, extLabel, extLock }:
    let
      c = lockConflicts { inherit baseLock extLock; };
      versionLines = map
        (v: "      ${v.name}: base=${formatList v.base} ext=${formatList v.ext}")
        c.versions;
      memberLine = "      ${formatList c.members}";
    in
    if c.versions == [ ] && c.members == [ ] then true
    else throw ''
      python-workspace: extension ${extLabel} is not lock-compatible with base ${baseLabel}
      ${lib.optionalString (c.versions != [ ]) ''
        version conflicts (package: base=X ext=Y):
      ${lib.concatStringsSep "\n" versionLines}
      ''}${lib.optionalString (c.members != [ ]) ''
        member-name collisions (rename the extension project):
      ${memberLine}
      ''}
      Fix: pin the extension to the base version (uv add '<name>==<base>' && uv lock),
      drop the package from the extension (it is already in the base lock), or use
      pythonWorkspace.replace to own the whole lock.
    '';
}
