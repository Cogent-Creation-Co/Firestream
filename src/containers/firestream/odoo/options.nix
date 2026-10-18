# Odoo Container Options (shared base)
# Copyright Firestream. MIT License.
#
# Externalized, declarative SHARED configuration for the Odoo container, consumed
# by bin/nix/firestream/containers/eval-container.nix. Defaults here are lifted
# VERBATIM from module.nix so that evalContainer's default build is byte-for-byte
# identical to the legacy flake.nix build path.
#
# NOTE: Odoo is MULTI-VERSION (15/16/17/18). `version` and `python` vary per
# build and are supplied by the flake-module via an inline override module, NOT
# here. This base module holds only the shared env/paths/ports/secrets.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, config, ... }:

let
  layout = import ./addons-layout.nix { inherit lib; };

  # Fields shared by EVERY addon source spec, whether it is a flat legacy
  # `vendoredAddons` entry or an ordered `addonLayers` entry. Declared once so
  # the two option types cannot drift; `addonLayerSpec` below adds fields to
  # this set rather than restating it.
  #
  # `src` (a derivation/path), when set, WINS over owner/repo/rev/hash so
  # non-GitHub sources work too — see `mkResolveSrc` in ./addons-layout.nix.
  addonSourceFields = {
    name = lib.mkOption {
      type = lib.types.str;
      description = "Label for this repo (used in build logs and collision errors).";
    };
    owner = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "GitHub owner (when fetching via fetchFromGitHub).";
    };
    repo = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "GitHub repo (when fetching via fetchFromGitHub).";
    };
    rev = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Commit or tag to pin (when fetching via fetchFromGitHub).";
    };
    hash = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "fetchFromGitHub sha256 (SRI string).";
    };
    src = lib.mkOption {
      type = lib.types.nullOr (lib.types.either lib.types.path lib.types.package);
      default = null;
      description = ''
        A prebuilt source tree (derivation or path). When set, it WINS over
        owner/repo/rev/hash, enabling non-GitHub sources (fetchgit, local path,
        flake input).
      '';
    };
    sourceRoot = lib.mkOption {
      type = lib.types.str;
      default = ".";
      description = "Subdirectory inside the repo that contains the module dirs.";
    };
    modules = lib.mkOption {
      type = lib.types.nullOr (lib.types.listOf lib.types.str);
      default = null;
      description = ''
        Explicit subset of module directory names to vendor. When null
        (default), every immediate child dir with an Odoo manifest
        (__manifest__.py / __openerp__.py) is auto-discovered.
      '';
    };
  };

  # Spec for one vendored addon repository. See ./vendor-addons.nix for how
  # these are turned into a baked /opt/firestream/odoo/vendor-addons/<module>
  # tree.
  vendoredAddonSpec = lib.types.submodule { options = addonSourceFields; };

  # Spec for one ORDERED addon layer. Everything a vendoredAddons entry has,
  # plus provenance/override policy. See ./addon-layers.nix.
  addonLayerSpec = lib.types.submodule {
    options = addonSourceFields // {
      shadows = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "sale_order_note" ];
        description = ''
          Module names this layer may legitimately override in a
          LOWER-precedence layer (including the implicit
          `legacy-vendor-addons` pseudo-layer holding
          `vendoredAddons`/`localAddons` output).

          Undeclared cross-layer duplicates are a hard build error. This list
          is the explicit, reviewable record of "yes, we meant to replace
          that", and it is checked at build time — a name listed here that no
          lower layer actually defines is simply inert.
        '';
      };
      autoInstall = lib.mkOption {
        type = lib.types.nullOr lib.types.bool;
        default = null;
        example = true;
        description = ''
          Whether this layer's modules join the default `odoo.installModules`
          (→ ODOO_INSTALL_MODULES → auto-install on boot).

          `null` (the default) means AUTO, and resolves against whether the
          module names are knowable at EVALUATION time. They are knowable when
          either `modules` is set explicitly, or `src` is a plain Nix path
          (a local directory Nix can read during evaluation). They are NOT
          knowable for a source fetched at build time — fetchFromGitHub, a
          derivation, a flake input.

          | `autoInstall` | names knowable | behaviour |
          |---------------|----------------|-----------|
          | `null`        | yes            | auto-install |
          | `null`        | no             | do NOT auto-install (silent) |
          | `true`        | yes            | auto-install |
          | `true`        | no             | evaluation error |
          | `false`       | either         | never auto-install |

          The silent `null`/not-knowable case is the correct default for
          OCA-style fetched layers: you pin a dependency repo, you do not want
          all 300 of its modules installed. Set `modules` explicitly if you do
          want a fetched layer auto-installed.
        '';
      };
    };
  };

  # Immediate child dirs of `dir` that carry an Odoo manifest — the same
  # discovery rule vendor-addons.nix applies at build time, evaluated here so
  # `installModules` can default to "everything under localAddons".
  discoverModules = dirs: lib.unique (lib.concatMap
    (dir: lib.attrNames (lib.filterAttrs
      (name: type: type == "directory" &&
        (builtins.pathExists (dir + "/${name}/__manifest__.py") ||
         builtins.pathExists (dir + "/${name}/__openerp__.py")))
      (builtins.readDir dir)))
    dirs);

  # Can we enumerate this layer's modules WITHOUT realising a derivation?
  # `builtins.isPath` is deliberately strict: it is true only for a real Nix
  # path value (`./addons`), which `builtins.readDir` can walk during
  # evaluation. A fetchFromGitHub result, any other derivation, and a flake
  # input attrset are all false — their contents do not exist yet.
  layerModulesKnowable = layer:
    layer.modules != null || (layer.src != null && builtins.isPath layer.src);

  # Module names a layer contributes to installModules (only ever called when
  # layerModulesKnowable is true).
  layerModules = layer:
    if layer.modules != null then layer.modules else discoverModules [ layer.src ];

  # Resolve `autoInstall` per the table in the option description above. The
  # `null` default NEVER throws; only an explicit `true` on an unknowable layer
  # does, and then with an actionable message.
  layerAutoInstall = layer:
    if layer.autoInstall == false then false
    else if layerModulesKnowable layer then true
    else if layer.autoInstall == true then
      throw ("odoo.addonLayers: layer '${layer.name}': autoInstall requires an "
        + "explicit `modules` list because its source is fetched at build time, "
        + "so its module names are not knowable during evaluation. Either set "
        + "`modules = [ ... ]` on this layer, or leave `autoInstall = null` and "
        + "install the modules some other way.")
    else false;
in
{
  # Build-time addon vendoring: a list of addon-repo specs baked into the image at
  # /opt/firestream/odoo/vendor-addons and appended to addons_path (see module.nix). Empty by
  # default — stock images are unchanged. Forwarded to the container factory via
  # `extraModuleArgs.vendoredAddons` below (the eval-container.nix seam).
  options.odoo.vendoredAddons = lib.mkOption {
    type = lib.types.listOf vendoredAddonSpec;
    default = [ ];
    description = ''
      Third-party Odoo addon repositories to vendor at build time. Each entry is
      fetched (GitHub by default, or any `src`), and its modules are laid into a
      baked read-only /opt/firestream/odoo/vendor-addons directory wired into addons_path.
    '';
  };

  # First-party addons, first class: directories of YOUR modules baked into the
  # image (via the vendoredAddons machinery) and auto-installed on boot (via
  # installModules → ODOO_INSTALL_MODULES → init.sh). The self-contained
  # "my company image" seam — no runtime mounts, no manual Apps-list install.
  options.odoo.localAddons = lib.mkOption {
    type = lib.types.listOf lib.types.path;
    default = [ ];
    example = lib.literalExpression "[ ./custom_addons ]";
    description = ''
      Directories of first-party Odoo modules to bake into the image. Every
      immediate child dir with an Odoo manifest (__manifest__.py /
      __openerp__.py) is vendored into /opt/firestream/odoo/vendor-addons
      (already on addons_path) and, by default, auto-installed on boot — see
      `odoo.installModules`.
    '';
  };

  # Ordered, provenance-carrying addon layers. THE mechanism for composing
  # tiers (e.g. a shared base tier, an org tier, a per-deployment tier) where a
  # higher tier deliberately overrides a module from a lower one — something the
  # flat `vendoredAddons` list cannot express, since any duplicate there is
  # fatal.
  #
  # ORDER IS SEMANTIC: the list runs base -> specific, LATER entries have HIGHER
  # precedence. Each layer is baked into its own directory
  # /opt/firestream/odoo/addons.d/<NN>-<name>/<module>, and addons_path lists
  # them most-specific-first (see ./addons-layout.nix). Layers sit AFTER Odoo
  # core and BEFORE the legacy vendor-addons directory.
  options.odoo.addonLayers = lib.mkOption {
    type = lib.types.listOf addonLayerSpec;
    default = [ ];
    example = lib.literalExpression ''
      [
        { name = "oca-web"; owner = "OCA"; repo = "web"; rev = "..."; hash = "...";
          modules = [ "web_responsive" ]; }
        { name = "org"; src = ./addons; }
        { name = "deployment"; src = ./client-addons; shadows = [ "my_module" ]; }
      ]
    '';
    description = ''
      Ordered Odoo addon layers, listed base -> specific: later entries take
      precedence over earlier ones.

      Each layer accepts the same source fields as `odoo.vendoredAddons`
      (name/owner/repo/rev/hash/src/sourceRoot/modules) plus `shadows` and
      `autoInstall`. Layers are baked to
      `/opt/firestream/odoo/addons.d/<NN>-<name>/<module>`, with `<NN>` the
      zero-padded declaration index, and are wired into `addons_path` in
      reverse (most specific first).

      A module defined by two layers is a BUILD ERROR unless the
      higher-precedence layer names it in `shadows`. The legacy
      `/opt/firestream/odoo/vendor-addons` output participates in that check as
      an implicit lowest-precedence layer called `legacy-vendor-addons`.

      NOTE: after changing layers on an EXISTING deployment you must also set
      `ODOO_FORCE_OVERWRITE_CONF=yes`, because `/opt/firestream/odoo/conf` is
      persistent and `odoo.conf` is only generated when absent — otherwise the
      container keeps its stale `addons_path` and the new layers are invisible.
    '';
  };

  # Where the runtime-overridable addons dir sits on addons_path.
  options.odoo.addonsDirPrecedence = lib.mkOption {
    type = lib.types.enum [ "first" "last" ];
    default = "last";
    example = "first";
    description = ''
      Position of `{{ODOO_ADDONS_DIR}}` (ODOO_ADDONS_DIR, the writable
      /opt/firestream/odoo/addons directory that charts and docker-compose bind
      mounts land in) within `addons_path`.

      `"last"` (the default) reproduces the historical ordering exactly: baked
      content wins, so baking and mounting the same module collides.

      `"first"` puts it at the FRONT, so a runtime bind mount shadows every
      baked layer. That is what lets one image serve both the production and
      the live-edit development loop instead of needing a second image.

      Odoo core is unaffected either way — it is never shadowable by this
      option, because core comes from `/opt/firestream/odoo/odoo/addons`.
    '';
  };

  options.odoo.installModules = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = lib.unique (
      lib.concatMap (l: if layerAutoInstall l then layerModules l else [ ])
        config.odoo.addonLayers
      ++ discoverModules config.odoo.localAddons
    );
    defaultText = lib.literalMD ''
      every module contributed by an `odoo.addonLayers` entry that resolves to
      auto-install (see `autoInstall`), unioned with every module discovered
      under `odoo.localAddons`
    '';
    example = [ "my_module" "base_fontawesome" ];
    description = ''
      Module names to install automatically, exported as ODOO_INSTALL_MODULES.
      The entrypoint appends them to `--init=base` on first boot and re-`--init`s
      them on subsequent boots, so modules newly baked into an image are also
      installed on existing databases (`--init` is idempotent: missing modules
      are installed, present ones updated).
    '';
  };

  # ---------------------------------------------------------------------------
  # THE gevent-port switch. Odoo has two server modes and they expose DIFFERENT
  # sockets:
  #
  #   workers = 0  ThreadedServer. ONE process, binds only http_port, and
  #                serves websocket/longpolling traffic ON THAT SAME PORT.
  #                gevent_port is written into odoo.conf and then ignored --
  #                nothing ever listens on it.
  #   workers > 0  PreforkServer. Spawns N HTTP workers plus a separate gevent
  #                worker, and THAT is what binds gevent_port (8072).
  #
  # So any deployment whose reverse proxy peels /websocket and /longpolling off
  # to 8072 -- which is the standard Odoo-behind-nginx layout, and what the
  # firestream nginx chart generates -- is broken unless this is > 0: every
  # websocket request gets connection-refused (502 through the proxy).
  #
  # The default stays 0 because that is the historical behaviour and it is the
  # right choice for the single-container docker-compose loop, where nothing
  # splits ports and one process is cheaper.
  # ---------------------------------------------------------------------------
  options.odoo.workers = lib.mkOption {
    type = lib.types.ints.unsigned;
    default = 0;
    example = 2;
    description = ''
      Number of Odoo HTTP worker processes (`workers` in odoo.conf), exported as
      ODOO_WORKERS.

      `0` runs Odoo in threaded mode: a single process bound to
      `ODOO_PORT_NUMBER` only, serving websockets on that port. **The gevent /
      longpolling port (`ODOO_LONGPOLLING_PORT_NUMBER`, 8072) is NOT bound in
      this mode.**

      Any value greater than `0` switches Odoo to prefork mode, which spawns the
      gevent worker and binds 8072. Set this whenever something -- typically a
      reverse proxy routing `/websocket` and `/longpolling` -- expects 8072 to
      answer.
    '';
  };

  config.odoo = {
    # Forward the vendored-addons list to module.nix through the factory's
    # extraModuleArgs seam (eval-container.nix splices this into moduleArgs).
    extraModuleArgs.vendoredAddons = config.odoo.vendoredAddons;

    # Same seam for the ordered layers and the addons_path precedence knob.
    # module.nix recomputes addons_path from exactly these two values via
    # ./addons-layout.nix, so the baked ODOO_ADDONS_PATH below and the
    # odoo.conf template can never disagree.
    extraModuleArgs.addonLayers = config.odoo.addonLayers;
    extraModuleArgs.addonsDirPrecedence = config.odoo.addonsDirPrecedence;

    # localAddons ride the vendoredAddons machinery: each dir becomes a spec
    # whose `src` wins over GitHub coordinates; vendor-addons.nix auto-discovers
    # the module dirs. List options merge by concatenation, so this composes
    # with consumer-supplied vendoredAddons.
    vendoredAddons = map
      (p: { name = "local-${builtins.baseNameOf p}"; src = p; })
      config.odoo.localAddons;

    # Paths configuration
    # Per-key mkDefault so individual paths can be overridden independently.
    # KEEP "/opt/firestream/odoo/log" exactly (do NOT normalise to "logs").
    paths = {
      base = lib.mkDefault "/opt/firestream/odoo";
      conf = lib.mkDefault "/opt/firestream/odoo/conf";
      data = lib.mkDefault "/firestream/odoo/data";
      logs = lib.mkDefault "/opt/firestream/odoo/log";
    };

    # Environment variables with defaults
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    # ODOO_VERSION tracks config.odoo.version (supplied per-build by the
    # flake-module's override module), matching the legacy `ODOO_VERSION = odooVersion`.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Paths
      ODOO_BASE_DIR = "/opt/firestream/odoo";
      ODOO_BIN_DIR = "/opt/firestream/odoo/bin";
      ODOO_CONF_DIR = "/opt/firestream/odoo/conf";
      ODOO_CONF_FILE = "/opt/firestream/odoo/conf/odoo.conf";
      ODOO_DATA_DIR = "/firestream/odoo/data";
      ODOO_ADDONS_DIR = "/opt/firestream/odoo/addons";

      # THE addons_path, rendered once by ./addons-layout.nix and baked into the
      # image so every generator reads one value instead of its own copy. It
      # deliberately still carries the literal {{ODOO_ADDONS_DIR}} token: the
      # runtime addons dir is overridable per-deployment (charts remap it), so
      # the substitution happens at container start — in module.nix's activateFn
      # sed pipeline for the template path, and in scripts/config.sh for the
      # fallback path.
      #
      # With `addonLayers = [ ]` and the default `addonsDirPrecedence` this is
      # byte-identical to the pre-addonLayers hardcoded literal.
      ODOO_ADDONS_PATH = layout.mkAddonsPath {
        layerDirs = layout.layerDirNames config.odoo.addonLayers;
        inherit (config.odoo) addonsDirPrecedence;
      };

      ODOO_TMP_DIR = "/opt/firestream/odoo/tmp";
      ODOO_PID_FILE = "/opt/firestream/odoo/tmp/odoo.pid";
      ODOO_LOGS_DIR = "/opt/firestream/odoo/log";
      ODOO_LOG_FILE = "/opt/firestream/odoo/log/odoo-server.log";

      # Volume paths
      ODOO_VOLUME_DIR = "/firestream/odoo";

      # User/group
      ODOO_DAEMON_USER = "odoo";
      ODOO_DAEMON_GROUP = "odoo";

      # Port configuration
      ODOO_PORT_NUMBER = "8069";
      ODOO_LONGPOLLING_PORT_NUMBER = "8072";

      # HTTP worker processes; see module.nix. 0 = threaded mode, and the gevent
      # port is NOT bound. Set > 0 (via config.odoo.workers) whenever anything
      # depends on ODOO_LONGPOLLING_PORT_NUMBER being reachable.
      ODOO_WORKERS = builtins.toString config.odoo.workers;

      # Bootstrap configuration
      ODOO_SKIP_BOOTSTRAP = "no";
      ODOO_SKIP_MODULES_UPDATE = "no";
      ODOO_LOAD_DEMO_DATA = "no";
      ODOO_LIST_DB = "no";
      # Comma-separated modules init.sh installs (see odoo.installModules).
      ODOO_INSTALL_MODULES = lib.concatStringsSep "," config.odoo.installModules;

      # Odoo credentials
      ODOO_EMAIL = "admin";
      ODOO_PASSWORD = "admin";

      # SMTP configuration
      ODOO_SMTP_HOST = "";
      ODOO_SMTP_PORT_NUMBER = "";
      ODOO_SMTP_USER = "";
      ODOO_SMTP_PASSWORD = "";
      ODOO_SMTP_PROTOCOL = "";

      # Database configuration
      ODOO_DATABASE_HOST = "postgresql";
      ODOO_DATABASE_PORT_NUMBER = "5432";
      ODOO_DATABASE_NAME = "firestream_odoo";
      ODOO_DATABASE_USER = "firestream";
      ODOO_DATABASE_PASSWORD = "";
      ODOO_DATABASE_FILTER = "";

      # Timeouts
      ODOO_DB_WAIT_TIMEOUT = "120";

      # Empty password flag — default to yes for out-of-the-box local/dev/e2e
      # use; production overrides via the standard mkDefault seam.
      ALLOW_EMPTY_PASSWORD = "yes";

      # Debug mode
      BITNAMI_DEBUG = "false";

      # Odoo version (for scripts) - tracks config.odoo.version
      ODOO_VERSION = config.odoo.version;
    };

    # Variables that support Docker secrets (_FILE suffix).
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "ODOO_PASSWORD"
      "ODOO_DATABASE_PASSWORD"
      "ODOO_SMTP_PASSWORD"
      "ODOO_SMTP_HOST"
      "ODOO_SMTP_PORT_NUMBER"
      "ODOO_SMTP_USER"
      "ODOO_SMTP_PROTOCOL"
      "ODOO_DATABASE_HOST"
      "ODOO_DATABASE_PORT_NUMBER"
      "ODOO_DATABASE_NAME"
      "ODOO_DATABASE_USER"
      "ODOO_DATABASE_FILTER"
      "ODOO_EMAIL"
      "ODOO_SKIP_BOOTSTRAP"
      "ODOO_SKIP_MODULES_UPDATE"
      "ODOO_LOAD_DEMO_DATA"
      "ODOO_LIST_DB"
      "ODOO_INSTALL_MODULES"
    ];

    exposedPorts = lib.mkDefault [ 8069 8072 ];

    # Phase 4: enable in-image firestream-healthd. Odoo's /web/database/selector
    # is publicly available once the http worker is up; we use curl --fail to
    # treat any non-2xx as not-yet-ready. `ODOO_PORT_NUMBER` defaults to 8069.
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''curl -fsS -o /dev/null "http://localhost:''${ODOO_PORT_NUMBER:-8069}/web/database/selector"'';
    };

    # Odoo requires a postgres metadata DB; embed it as a dependency sub-service
    # so `.#odoo-up` is a working out-of-the-box stack. Mirrors the airflow /
    # superset pattern. Whole-block mkDefault: a consumer override supplies its
    # own complete topology.
    compose = lib.mkDefault {
      projectName = "firestream-odoo";
      dependencies = [ "postgresql" ];

      # +34000 host-port offset. Each of the 8 canonical apps gets a DISTINCT
      # offset (spacing 2000) so all 8 can run simultaneously on docker without
      # host-port collisions. odoo=34000.
      #   postgresql 5432 -> host 39432
      #   odoo       8069 -> host 42069
      #   odoo gevent 8072 -> host 42072
      #   healthd    9180 -> host 43180
      hostPortOffset = 34000;

      sharedEnv = {
        ODOO_DATABASE_HOST = "postgresql";
        ODOO_DATABASE_NAME = "firestream_odoo";
        ODOO_DATABASE_USER = "odoo";
        ODOO_DATABASE_PASSWORD = "odoo";
      };

      # Both volumes must be NAMED. An image VOLUME with no compose mapping gets
      # an anonymous volume, which is discarded when the container is recreated
      # -- while a named postgresql_data survives. That split lifecycle is what
      # desynchronises Odoo's two halves: the DB keeps ir_attachment rows whose
      # store_fname files went with the orphaned filestore volume.
      volumes = {
        postgresql_data = { };
        odoo_data = { };
      };

      services = {
        postgresql = {
          image = "@postgresql";
          env = {
            POSTGRESQL_DATABASE = "firestream_odoo";
            POSTGRESQL_USERNAME = "odoo";
            POSTGRESQL_PASSWORD = "odoo";
            ALLOW_EMPTY_PASSWORD = "no";
          };
          ports = [ "5432:5432" ];
          volumes = [ "postgresql_data:/firestream/postgresql" ];
          healthcheck = {
            test = [
              "CMD"
              "bash"
              "-c"
              "exec 3<>/dev/tcp/127.0.0.1/9180 && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
            ];
            interval = "10s";
            timeout = "5s";
            retries = 5;
            start_period = "30s";
          };
        };
        odoo = {
          # Own firestream-odoo image; publish web + longpolling + healthd.
          ports = [ "8069:8069" "8072:8072" "9180:9180" ];
          dependsOn = [ "postgresql" ];
          # Mount the parent, matching the image VOLUME in module.nix and the
          # chart's mountPath. Covers filestore + .app_initialized + .state.
          volumes = [ "odoo_data:/firestream/odoo" ];
        };
      };
    };
  };
}
