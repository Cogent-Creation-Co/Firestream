# Odoo Addon LAYERS Builder
# Copyright Firestream. MIT License.
#
# Build-time vendoring of ORDERED, PROVENANCE-CARRYING addon layers
# (`config.odoo.addonLayers`). Where ./vendor-addons.nix flattens every spec
# into one directory and hard-fails on any duplicate module name, this builder
# gives every layer its OWN directory:
#
#   $out/opt/firestream/odoo/addons.d/<NN>-<name>/<module>/...
#         -> image /opt/firestream/odoo/addons.d/<NN>-<name>/<module>/...
#
# `<NN>` is the zero-padded DECLARED index (see ./addons-layout.nix), so
# `ls addons.d` reads base -> specific and the on-disk order matches the
# `odoo.addonLayers` list. addons_path lists these directories in the REVERSE
# order (most specific first) because Odoo takes the first match — see
# ./addons-layout.nix `mkAddonsPath`.
#
# ---------------------------------------------------------------------------
# WHY THE COLLISION CHECK RUNS IN BASH AND NOT IN NIX
# ---------------------------------------------------------------------------
# A layer with `modules = null` and a fetched source has module names that do
# not exist until the source is realised — there is nothing for Nix to inspect
# at evaluation time. So the check lives in the build script, where every
# layer's modules are on disk.
#
# The check is PURELY DIAGNOSTIC: each layer owns a separate directory, so a
# collision never overwrites anything. It exists so that an accidental override
# is a loud build failure rather than a silent behaviour change six months
# later. Algorithm:
#
#   iterate layers HIGHEST precedence -> LOWEST (then the legacy pseudo-layer)
#   seen[module] := the highest-precedence layer that defines it
#   on collision, permit iff the ALREADY-SEEN (winning) layer listed that
#   module in its `shadows`; otherwise fail, naming BOTH layers.
#
# The legacy /opt/firestream/odoo/vendor-addons output (from `vendoredAddons` /
# `localAddons`) participates as an implicit lowest-precedence pseudo-layer
# named `legacy-vendor-addons`, so a new layer colliding with a legacy module is
# diagnosed exactly like any other collision instead of silently winning.
#
# A `layers.json` is written next to the layer dirs recording the resolved
# order, each layer's declared `shadows`, and each layer's realised modules.
# It is a debugging artifact; `addons.d` itself is NOT on addons_path (only its
# `<NN>-<name>` children are), so the file is inert to Odoo.
#
# Usage (from module.nix):
#   addonLayersDrv = import ./addon-layers.nix { inherit pkgs lib; } {
#     version   = odooVersion;
#     layers    = addonLayers;                     # ordered base -> specific
#     legacyDrv = vendoredAddonsDrv;               # or null
#   };

{ pkgs, lib }:

{ version
, layers ? [ ]
  # The ./vendor-addons.nix derivation, so its modules can join the collision
  # check as `legacy-vendor-addons`. null when no legacy specs are configured.
, legacyDrv ? null
}:

let
  layout = import ./addons-layout.nix { inherit lib; };
  resolveSrc = layout.mkResolveSrc pkgs;

  dirNames = layout.layerDirNames layers;

  # Declared order (base -> specific). index 0 = lowest precedence.
  indexed = lib.imap0 (i: spec: { inherit i spec; dir = builtins.elemAt dirNames i; }) layers;

  # --- copy phase -----------------------------------------------------------
  copyCall = { i, spec, dir }:
    let
      src = resolveSrc spec;
      srcRoot = spec.sourceRoot or ".";
      mode = if (spec.modules or null) == null then "auto" else "explicit";
      mods = if (spec.modules or null) == null then [ ] else spec.modules;
    in
    "copy_layer ${lib.escapeShellArg spec.name} ${lib.escapeShellArg dir} "
    + "${lib.escapeShellArg "${src}/${srcRoot}"} ${mode} ${lib.escapeShellArgs mods}\n";

  # --- shadow declarations (eval-time known) --------------------------------
  shadowCalls = lib.concatMapStrings
    ({ spec, ... }: lib.concatMapStrings
      (m: "shadow[${lib.escapeShellArg "${spec.name}|${m}"}]=1\n")
      (spec.shadows or [ ]))
    indexed;

  # --- collision check, HIGHEST precedence first ----------------------------
  checkCalls = lib.concatMapStrings
    ({ spec, dir, ... }: "check_layer ${lib.escapeShellArg spec.name} ${lib.escapeShellArg dir}\n")
    (lib.reverseList indexed);

  # --- layers.json ----------------------------------------------------------
  jsonCalls = lib.concatMapStrings
    ({ i, spec, dir }:
      "add_layer_json ${toString i} ${lib.escapeShellArg dir} ${lib.escapeShellArg spec.name} "
      + "${lib.escapeShellArgs (spec.shadows or [ ])}\n")
    indexed;
in
pkgs.runCommand "odoo-addon-layers-${version}"
{
  meta.description = "Ordered Odoo addon layers baked into /opt/firestream/odoo/addons.d";
}
  ''
    shopt -s nullglob
    outRoot="$out/opt/firestream/odoo/addons.d"
    mkdir -p "$outRoot"

    ############################################################################
    # helpers
    ############################################################################

    # copy_layer <label> <dir> <srcdir> <auto|explicit> [modules...]
    copy_layer() {
      local label="$1" dir="$2" srcdir="$3" mode="$4"
      shift 4
      local dest="$outRoot/$dir"
      echo "[addon-layers] $dir ($label): $srcdir"
      if [[ ! -d "$srcdir" ]]; then
        echo "[addon-layers] ERROR: sourceRoot not found for layer '$label': $srcdir" >&2
        exit 1
      fi
      mkdir -p "$dest"

      local found=0 d mod
      if [[ "$mode" == "auto" ]]; then
        # Auto-discover: every immediate child dir with an Odoo manifest.
        # Same rule as vendor-addons.nix and options.nix `discoverModules`.
        for d in "$srcdir"/*/; do
          [[ -d "$d" ]] || continue
          if [[ -f "$d/__manifest__.py" || -f "$d/__openerp__.py" ]]; then
            mod="$(basename "$d")"
            cp -r "$d" "$dest/$mod"
            found=1
          fi
        done
        if [[ "$found" -eq 0 ]]; then
          echo "[addon-layers] ERROR: no Odoo modules found in layer '$label' ($srcdir)" >&2
          exit 1
        fi
      else
        for mod in "$@"; do
          if [[ ! -d "$srcdir/$mod" ]]; then
            echo "[addon-layers] ERROR: module '$mod' not found in layer '$label' ($srcdir)" >&2
            exit 1
          fi
          cp -r "$srcdir/$mod" "$dest/$mod"
        done
      fi
    }

    # modlist <dir> -> module names, one per line, sorted
    modlist() {
      local d
      for d in "$1"/*/; do
        [[ -d "$d" ]] || continue
        basename "$d"
      done | LC_ALL=C sort
    }

    # check_layer <label> <dir>   (callers go highest precedence -> lowest)
    check_layer() {
      local label="$1" dir="$2"
      local mod prev key
      while read -r mod; do
        [[ -n "$mod" ]] || continue
        prev="''${seen[$mod]:-}"
        if [[ -n "$prev" ]]; then
          key="$prev|$mod"
          if [[ -z "''${shadow[$key]:-}" ]]; then
            echo "" >&2
            echo "[addon-layers] ERROR: addon layer collision on module '$mod'." >&2
            echo "  higher precedence: layer '$prev'" >&2
            echo "  lower  precedence: layer '$label'" >&2
            echo "" >&2
            echo "  Layers are ordered base -> specific, so '$prev' would win at runtime" >&2
            echo "  and the copy in '$label' would never be loaded. If that override" >&2
            echo "  is intentional, declare it on the WINNING layer:" >&2
            echo "" >&2
            echo "      { name = \"$prev\"; ...; shadows = [ \"$mod\" ]; }" >&2
            echo "" >&2
            exit 1
          fi
          echo "[addon-layers] '$prev' shadows '$mod' from '$label' (declared)"
        else
          seen[$mod]="$label"
        fi
      done < <(modlist "$outRoot/$dir")
    }

    # check_legacy <dir>  — the implicit lowest-precedence pseudo-layer.
    check_legacy() {
      local label="legacy-vendor-addons" dir="$1"
      local mod prev key
      [[ -d "$dir" ]] || return 0
      while read -r mod; do
        [[ -n "$mod" ]] || continue
        prev="''${seen[$mod]:-}"
        if [[ -n "$prev" ]]; then
          key="$prev|$mod"
          if [[ -z "''${shadow[$key]:-}" ]]; then
            echo "" >&2
            echo "[addon-layers] ERROR: addon layer collision on module '$mod'." >&2
            echo "  higher precedence: layer '$prev'" >&2
            echo "  lower  precedence: layer '$label'" >&2
            echo "" >&2
            echo "  '$label' is the implicit lowest-precedence pseudo-layer holding" >&2
            echo "  everything built from odoo.vendoredAddons / odoo.localAddons into" >&2
            echo "  /opt/firestream/odoo/vendor-addons. If overriding it is intentional," >&2
            echo "  declare it on the WINNING layer:" >&2
            echo "" >&2
            echo "      { name = \"$prev\"; ...; shadows = [ \"$mod\" ]; }" >&2
            echo "" >&2
            exit 1
          fi
          echo "[addon-layers] '$prev' shadows '$mod' from '$label' (declared)"
        else
          seen[$mod]="$label"
        fi
      done < <(modlist "$dir")
    }

    # json_array [items...] -> ["a","b"]
    json_array() {
      local out="[" first=1 v
      for v in "$@"; do
        [[ "$first" -eq 1 ]] || out+=","
        first=0
        v="''${v//\\/\\\\}"
        v="''${v//\"/\\\"}"
        out+="\"$v\""
      done
      printf '%s]' "$out"
    }

    LAYER_JSON=()
    # add_layer_json <index> <dir> <name> [shadows...]
    add_layer_json() {
      local idx="$1" dir="$2" name="$3"
      shift 3
      local shadows_json mods_json
      shadows_json="$(json_array "$@")"
      local mods=()
      mapfile -t mods < <(modlist "$outRoot/$dir")
      mods_json="$(json_array ''${mods[@]+"''${mods[@]}"})"
      LAYER_JSON+=("    { \"index\": $idx, \"precedence\": $idx, \"dir\": \"$dir\", \"name\": \"$name\", \"shadows\": $shadows_json, \"modules\": $mods_json }")
    }

    ############################################################################
    # 1. materialise every layer into its own directory (declared order)
    ############################################################################
    ${lib.concatMapStrings copyCall indexed}

    ############################################################################
    # 2. collision / shadow check, highest precedence first
    ############################################################################
    declare -A seen
    declare -A shadow
    ${shadowCalls}
    ${checkCalls}
    ${if legacyDrv == null then "" else ''check_legacy "${legacyDrv}/opt/firestream/odoo/vendor-addons"''}

    ############################################################################
    # 3. layers.json (debugging artifact; higher index = higher precedence)
    ############################################################################
    ${jsonCalls}
    {
      printf '{\n'
      printf '  "comment": "Resolved odoo.addonLayers. Declared order, base -> specific; higher index wins. addons_path lists these in reverse.",\n'
      printf '  "layers": [\n'
      first=1
      for ((i = 0; i < ''${#LAYER_JSON[@]}; i++)); do
        if [[ "$first" -eq 1 ]]; then first=0; else printf ',\n'; fi
        printf '%s' "''${LAYER_JSON[$i]}"
      done
      [[ "$first" -eq 1 ]] || printf '\n'
      printf '  ],\n'
      ${if legacyDrv == null then ''
      printf '  "legacy": null\n'
      '' else ''
      legacy_mods=()
      mapfile -t legacy_mods < <(modlist "${legacyDrv}/opt/firestream/odoo/vendor-addons")
      printf '  "legacy": { "name": "legacy-vendor-addons", "dir": "/opt/firestream/odoo/vendor-addons", "modules": %s }\n' \
        "$(json_array ''${legacy_mods[@]+"''${legacy_mods[@]}"})"
      ''}
      printf '}\n'
    } > "$outRoot/layers.json"

    # Make copies writable: store sources are read-only, but Odoo/runtime tools
    # may stat/touch module files. Mirrors vendor-addons.nix.
    chmod -R u+w "$outRoot"
  ''
