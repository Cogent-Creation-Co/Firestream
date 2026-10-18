# Odoo Addons Layout — THE single source of truth for `addons_path`
# Copyright Firestream. MIT License.
#
# Everything that needs to know WHERE Odoo addons live, and in WHAT ORDER Odoo
# should search them, comes from this file. There are three consumers and they
# must never drift:
#
#   1. ./options.nix   — bakes the rendered path into the image env as
#                        ODOO_ADDONS_PATH (carrying the {{ODOO_ADDONS_DIR}}
#                        token, unsubstituted).
#   2. ./module.nix    — renders `addons_path = ...` into odoo.conf.template,
#                        and derives the per-layer output directory names.
#   3. ./scripts/config.sh — its fallback conf generator reads ODOO_ADDONS_PATH
#                        and applies the same {{ODOO_ADDONS_DIR}} substitution
#                        the template pipeline applies.
#
# Historically (1)/(2)/(3) were three hardcoded copies of the same string; the
# third one (config.sh) silently diverged the moment anything was added. Do not
# re-introduce a literal addons_path anywhere: call `mkAddonsPath`.
#
# ---------------------------------------------------------------------------
# ORDERING CONTRACT
# ---------------------------------------------------------------------------
#
#   [{{ODOO_ADDONS_DIR}}]                       when addonsDirPrecedence == "first"
#   <baseDir>/addons                            Odoo core-adjacent / runtime addons dir
#   <baseDir>/odoo/addons                       Odoo core addons
#   <baseDir>/addons.d/<NN>-<name>              addonLayers, HIGHEST precedence first
#   ...                                         ... down to the lowest
#   <baseDir>/vendor-addons                     legacy vendoredAddons/localAddons output
#   [{{ODOO_ADDONS_DIR}}]                       when addonsDirPrecedence == "last" (default)
#
# Odoo resolves a module name by scanning addons_path LEFT TO RIGHT and taking
# the FIRST hit. Two deliberate consequences:
#
#   * Layers come AFTER Odoo core. A layer therefore cannot silently shadow a
#     core Odoo addon — core is not a layer, so the `shadows` diagnostic in
#     ./addon-layers.nix could never see such a collision. Shadowing core is out
#     of scope; if you need it, fork the module under a new name.
#   * The legacy `vendor-addons` directory is the LOWEST-precedence external
#     source, so introducing `addonLayers` can only ever add precedence above
#     what already existed. Existing images are unaffected.
#
# With `layerDirs = [ ]` and the default precedence the rendered string is
# byte-identical to the pre-addonLayers hardcoded literal. That is a hard
# backwards-compatibility guarantee — see the `odoo-addons-path-legacy` check.

{ lib }:

rec {
  # Where the Odoo app tree lives in the image. Matches options.nix
  # `paths.base` / ODOO_BASE_DIR.
  defaultBaseDir = "/opt/firestream/odoo";

  # The placeholder the odoo.conf template carries for the runtime-overridable
  # addons dir. activateFn (module.nix) and config.sh both sed this away.
  addonsDirToken = "{{ODOO_ADDONS_DIR}}";

  # Resolve one addon spec's source tree: an explicit `src` (derivation or path)
  # WINS over owner/repo/rev/hash so non-GitHub sources (fetchgit, local path,
  # flake input) work too. Curried on `pkgs` so this file stays pure and
  # importable from options.nix, which has no pkgs in scope.
  mkResolveSrc = pkgs: spec:
    if (spec.src or null) != null
    then spec.src
    else pkgs.fetchFromGitHub {
      owner = spec.owner;
      repo = spec.repo;
      rev = spec.rev;
      sha256 = spec.hash;
    };

  # Directory name for layer #i. `<NN>` is the ZERO-PADDED index in DECLARED
  # order (base -> specific), so:
  #   * `ls addons.d` sorts into declaration order and precedence is legible
  #     on disk without consulting the flake;
  #   * the name is stable across rebuilds (it is not content-derived), so an
  #     unrelated layer edit does not renumber its siblings.
  layerDirName = i: spec: "${lib.fixedWidthNumber 2 i}-${spec.name}";

  # Declared-order list of layer directory names, with a hard uniqueness check:
  # two layers sharing a `name` would map to distinct directories (different
  # index) but produce confusing diagnostics and an ambiguous layers.json.
  layerDirNames = specs:
    let
      names = map (s: s.name) specs;
      dups = lib.unique (lib.filter (n: lib.count (m: m == n) names > 1) names);
    in
    if dups != [ ]
    then throw ("odoo.addonLayers: duplicate layer name(s): "
      + lib.concatStringsSep ", " dups
      + ". Layer names must be unique — they identify the layer in collision "
      + "diagnostics and become its addons.d directory.")
    else lib.imap0 layerDirName specs;

  # Render addons_path. `layerDirs` is in DECLARED order (base -> specific);
  # this function reverses it, because Odoo takes the first match and the most
  # specific layer must win.
  mkAddonsPath =
    { layerDirs ? [ ]
    , addonsDirPrecedence ? "last"
    , baseDir ? defaultBaseDir
    , runtimeAddonsDir ? addonsDirToken
    }:
    let
      core = [ "${baseDir}/addons" "${baseDir}/odoo/addons" ];
      layers = map (d: "${baseDir}/addons.d/${d}") (lib.reverseList layerDirs);
      legacy = [ "${baseDir}/vendor-addons" ];
      runtime = [ runtimeAddonsDir ];
    in
    lib.concatStringsSep "," (
      lib.optionals (addonsDirPrecedence == "first") runtime
      ++ core
      ++ layers
      ++ legacy
      ++ lib.optionals (addonsDirPrecedence == "last") runtime
    );
}
