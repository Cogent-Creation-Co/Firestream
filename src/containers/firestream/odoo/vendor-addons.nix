# Odoo Vendored-Addons Builder
# Copyright Firestream. MIT License.
#
# Build-time vendoring of third-party Odoo addon repositories (OCA, etc.). Takes a
# list of addon-repo specs and produces a SINGLE derivation that lays every module
# out under `$out/opt/firestream/odoo/vendor-addons/<module>`. Because the container factory
# adds this derivation to `extraDeps` and the image builder links `/opt` into the
# rootfs (see bin/nix/firestream/containers/base.nix), the modules land at
# `/opt/firestream/odoo/vendor-addons/<module>` in the image — a baked, read-only addons dir
# that module.nix appends to `addons_path`.
#
# This is the headline "inject your preferences, get a custom image" capability:
# a downstream flake supplies `config.odoo.vendoredAddons = [ ... ]` and gets an
# Odoo image with those modules baked in, without forking Firestream.
#
# SCOPE: this builder is the FLAT, legacy path. It has exactly one output
# directory and any duplicate module name is fatal. If you need ORDERED tiers
# where a higher tier deliberately overrides a module from a lower one, use
# `config.odoo.addonLayers` and ./addon-layers.nix instead. Both derivations are
# produced and both land in the image; `vendor-addons` sits at the LOWEST
# external precedence on addons_path (see ./addons-layout.nix), so adding layers
# never changes what an existing vendoredAddons-only image resolves.
#
# Layout invariant: `$out/opt/firestream/odoo/vendor-addons/<module>/...`  ->  image
# `/opt/firestream/odoo/vendor-addons/<module>/...`. Mirrors the `source.nix` idiom of
# installing into `$out/opt/firestream/odoo`.
#
# Usage (from module.nix):
#   vendoredAddonsDrv = import ./vendor-addons.nix { inherit pkgs lib; } {
#     version = odooVersion;            # e.g. "18.0"
#     specs   = vendoredAddons;         # list of resolved option submodules
#   };
#
# Spec shape (each list entry — see options.nix `vendoredAddons` type):
#   { name; owner; repo; rev; hash; src ? null; sourceRoot ? "."; modules ? null; }
#   - `src` (a derivation/path), when non-null, WINS over owner/repo/rev/hash so
#     non-GitHub sources (fetchgit, local path, flake input) work too.
#   - `sourceRoot` is the subdir inside the repo holding the module dirs.
#   - `modules = null` auto-discovers every immediate child dir that carries a
#     `__manifest__.py` (or legacy `__openerp__.py`); a list copies exactly those.

{ pkgs, lib }:

{ version, specs }:

let
  # Resolve each spec's source tree: explicit `src` wins, else fetchFromGitHub.
  # Shared verbatim with ./addon-layers.nix via ./addons-layout.nix so the two
  # builders can never disagree about what a spec's source is.
  resolveSrc = (import ./addons-layout.nix { inherit lib; }).mkResolveSrc pkgs;

  # Per-spec shell snippet that copies the chosen modules into the output.
  copySnippet = spec:
    let
      src = resolveSrc spec;
      srcRoot = spec.sourceRoot or ".";
      label = spec.name or "${spec.owner or "?"}/${spec.repo or "?"}";
      explicitModules = spec.modules or null;
      modulesArg =
        if explicitModules == null then ""
        else lib.concatStringsSep " " explicitModules;
    in ''
      echo "[vendor-addons] ${label}: ${src}/${srcRoot}"
      srcdir="${src}/${srcRoot}"
      if [[ ! -d "$srcdir" ]]; then
        echo "[vendor-addons] ERROR: sourceRoot '${srcRoot}' not found in ${label}" >&2
        exit 1
      fi

      ${if explicitModules == null then ''
      # Auto-discover: every immediate child dir with an Odoo manifest.
      found=0
      for d in "$srcdir"/*/; do
        [[ -d "$d" ]] || continue
        if [[ -f "$d/__manifest__.py" || -f "$d/__openerp__.py" ]]; then
          mod="$(basename "$d")"
          if [[ -e "$out/opt/firestream/odoo/vendor-addons/$mod" ]]; then
            echo "[vendor-addons] ERROR: duplicate module '$mod' (from ${label})" >&2
            exit 1
          fi
          cp -r "$d" "$out/opt/firestream/odoo/vendor-addons/$mod"
          found=1
        fi
      done
      if [[ "$found" -eq 0 ]]; then
        echo "[vendor-addons] ERROR: no Odoo modules found in ${label} (${srcRoot})" >&2
        exit 1
      fi
      '' else ''
      # Explicit module subset.
      for mod in ${modulesArg}; do
        if [[ ! -d "$srcdir/$mod" ]]; then
          echo "[vendor-addons] ERROR: module '$mod' not found in ${label}" >&2
          exit 1
        fi
        if [[ -e "$out/opt/firestream/odoo/vendor-addons/$mod" ]]; then
          echo "[vendor-addons] ERROR: duplicate module '$mod' (from ${label})" >&2
          exit 1
        fi
        cp -r "$srcdir/$mod" "$out/opt/firestream/odoo/vendor-addons/$mod"
      done
      ''}
    '';
in
pkgs.runCommand "odoo-vendored-addons-${version}"
  {
    meta.description = "Vendored Odoo addons baked into /opt/firestream/odoo/vendor-addons";
  }
  ''
    mkdir -p "$out/opt/firestream/odoo/vendor-addons"
    ${lib.concatMapStrings copySnippet specs}
    # Make copies writable: store sources are read-only, but Odoo/runtime tools
    # may stat/touch module files. Bytecode writes are disabled via
    # PYTHONDONTWRITEBYTECODE, so this is just defensive.
    chmod -R u+w "$out/opt/firestream/odoo/vendor-addons"
  ''
