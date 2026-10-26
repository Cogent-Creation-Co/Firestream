# Firestream host-port registry rules
# Copyright Firestream. MIT License.
#
# NOTE: every rule fails at evaluation time and names the offending rows, so
# `nix flake check --no-build` catches a bad edit to ports.nix without a
# build, and a compose package never renders from a bad table.
{ lib }:

let
  registryFile = "bin/nix/firestream/ports.nix";

  minPort = 1024;
  maxPort = 32767;

  label = e: "${e.stack}/${e.service}:${toString e.container}";

  rowShapeError = row:
    let
      ok = builtins.isList row
        && builtins.length row == 4
        && builtins.isString (builtins.elemAt row 0)
        && builtins.isString (builtins.elemAt row 1)
        && builtins.isInt (builtins.elemAt row 2)
        && builtins.isInt (builtins.elemAt row 3);
    in
    lib.optional (!ok)
      "row ${builtins.toJSON row} is not [ \"stack\" \"service\" containerPort hostPort ]";

  toEntry = row: {
    stack = builtins.elemAt row 0;
    service = builtins.elemAt row 1;
    container = builtins.elemAt row 2;
    host = builtins.elemAt row 3;
  };

  # Groups of entries that share `keyOf`, with more than one member.
  clashes = keyOf: entries:
    lib.filter (g: builtins.length g > 1)
      (builtins.attrValues (builtins.groupBy keyOf entries));

  registryErrors = registry:
    let
      shapeErrors = lib.concatMap rowShapeError registry.rows;
      entries = map toEntry registry.rows;
      stacks = lib.unique (map (e: e.stack) entries);
    in
    if shapeErrors != [ ] then shapeErrors
    else
      map
        (g: "host port ${toString (lib.head g).host} is used twice: ${lib.concatMapStringsSep ", " label g}")
        (clashes (e: toString e.host) entries)
      ++ map
        (g: "${label (lib.head g)} has ${toString (builtins.length g)} rows")
        (clashes label entries)
      ++ map
        (e: "host port ${toString e.host} of ${label e} is outside ${toString minPort}-${toString maxPort}")
        (lib.filter (e: e.host < minPort || e.host > maxPort) entries)
      ++ map
        (k: "stackOf.${k} names stack \"${registry.stackOf.${k}}\", which has no rows")
        (lib.filter (k: !(lib.elem registry.stackOf.${k} stacks)) (builtins.attrNames registry.stackOf));

  failWith = what: errors:
    throw "${registryFile}: ${what}:\n  ${lib.concatStringsSep "\n  " errors}";

  mkRegistry = registry:
    let
      errors = registryErrors registry;
      entries =
        if errors == [ ] then map toEntry registry.rows
        else failWith "the registry breaks its rules" errors;
      byKey = lib.listToAttrs (map (e: lib.nameValuePair (label e) e) entries);
    in
    rec {
      inherit entries;

      # The registry stack of an image key.
      stackOf = imageKey: registry.stackOf.${imageKey} or imageKey;

      hostPort = { stack, service, container }:
        let k = label { inherit stack service container; };
        in byKey.${k}.host or (failWith "a published port has no row" [
          "${k}: add [ \"${stack}\" \"${service}\" ${toString container} <hostPort> ] to the table"
        ]);

      # `published` is every { stack; service; container; } that a compose
      # package publishes; `imageKeys` is every image key of the flake.
      coverageErrors = { published, imageKeys }:
        let
          publishedKeys = lib.unique (map label published);
          missing = lib.filter (k: !(byKey ? ${k})) publishedKeys;
          stale = lib.filter (e: !(lib.elem (label e) publishedKeys)) entries;
          staleStackOf = lib.filter (k: !(lib.elem k imageKeys)) (builtins.attrNames registry.stackOf);
        in
        map (k: "${k} is published but has no row") missing
        ++ map (e: "row ${label e} -> ${toString e.host} is stale: no compose stack publishes it") stale
        ++ map (k: "stackOf.${k} is stale: no image has the key \"${k}\"") staleStackOf;

      checkCoverage = args:
        let errs = coverageErrors args;
        in if errs == [ ] then true else failWith "the registry and the compose stacks disagree" errs;

      markdownTable =
        let
          row = e: "| ${e.stack} | ${e.service} | ${toString e.container} | ${toString e.host} | ${if e.host == e.container then "yes" else ""} |";
        in
        lib.concatStringsSep "\n" ([
          "| Stack | Service | Container port | Host port | Canonical |"
          "|---|---|---|---|---|"
        ] ++ map row entries) + "\n";

      # The text between the begin and end markers of `doc`, or null.
      tableIn = doc:
        let
          parts = lib.splitString "<!-- port-table:begin -->\n" doc;
          after = if builtins.length parts == 2 then builtins.elemAt parts 1 else null;
          inner = if after == null then [ ] else lib.splitString "<!-- port-table:end -->" after;
        in
        if builtins.length inner == 2 then lib.head inner else null;

      inherit registryErrors;
    };
in
{
  inherit mkRegistry registryErrors;
  registry = mkRegistry (import ./ports.nix);
}
