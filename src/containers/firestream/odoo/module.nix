# Odoo Container Module - Using Firestream Factories
# Copyright Firestream. MIT License.
#
# This module defines the Odoo container using mkPythonContainerModule.
# Much simpler than manual implementation - the factory handles:
# - Entrypoint generation
# - Docker image building
# - Environment loading and secrets
# - Development shell
#
# Usage:
#   odooModule = import ./module.nix {
#     inherit pkgs lib firestream pythonEnv odooVersion python odooSource;
#   };

{ pkgs
, lib
, firestream
, pythonEnv          # The virtual environment from uv2nix
, odooVersion        # e.g., "18.0"
, python ? pkgs.python312
, odooSource         # The Odoo source derivation

# Build-time vendored addon repos (list of specs from options.odoo.vendoredAddons,
# forwarded via extraModuleArgs). Empty ⇒ no vendoring, stock image unchanged.
, vendoredAddons ? [ ]

# Ordered addon layers (options.odoo.addonLayers, forwarded via
# extraModuleArgs). Listed base -> specific; later entries win. Empty ⇒ no
# addons.d content and an addons_path byte-identical to the pre-layers literal.
, addonLayers ? [ ]

# Where {{ODOO_ADDONS_DIR}} sits on addons_path (options.odoo.addonsDirPrecedence).
# "last" is the historical ordering.
, addonsDirPrecedence ? "last"

# Externalized core-surface config. Defaults below are EXACTLY today's literals
# so the legacy flake.nix path (which does not pass these) and evalContainer
# (which passes the same values from options.nix) yield identical factory args.

# Paths configuration
, paths ? {
    base = "/opt/firestream/odoo";
    conf = "/opt/firestream/odoo/conf";
    data = "/firestream/odoo/data";
    logs = "/opt/firestream/odoo/log";
  }

# Environment variables with defaults
, envVars ? {
    # Paths
    ODOO_BASE_DIR = "/opt/firestream/odoo";
    ODOO_BIN_DIR = "/opt/firestream/odoo/bin";
    ODOO_CONF_DIR = "/opt/firestream/odoo/conf";
    ODOO_CONF_FILE = "/opt/firestream/odoo/conf/odoo.conf";
    ODOO_DATA_DIR = "/firestream/odoo/data";
    ODOO_ADDONS_DIR = "/opt/firestream/odoo/addons";
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

    # HTTP worker processes. 0 = threaded mode (one process; websockets are
    # served on ODOO_PORT_NUMBER and NOTHING BINDS the gevent port). Any value
    # > 0 puts Odoo in prefork mode, which is the only mode that spawns the
    # gevent worker and therefore the only mode in which
    # ODOO_LONGPOLLING_PORT_NUMBER is actually listening. A reverse proxy that
    # splits /websocket off to the gevent port MUST set this > 0.
    ODOO_WORKERS = "0";

    # Bootstrap configuration
    ODOO_SKIP_BOOTSTRAP = "no";
    ODOO_SKIP_MODULES_UPDATE = "no";
    ODOO_LOAD_DEMO_DATA = "no";
    ODOO_LIST_DB = "no";

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
    # use; production overrides via the standard envVars override seam.
    ALLOW_EMPTY_PASSWORD = "yes";

    # Debug mode
    BITNAMI_DEBUG = "false";

    # Odoo version (for scripts)
    ODOO_VERSION = odooVersion;
  }

# Variables that support Docker secrets (_FILE suffix)
, envVarsWithSecrets ? [
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
  ]

, exposedPorts ? [ 8069 8072 ]

# In-image health/SBOM service configuration (Phase 4). Forwarded to
# mkPythonContainerModule (which forwards to mkContainerModule). Default-off
# preserves byte-identical legacy-flake behaviour.
, health ? { enable = false; port = 9180; readinessCmd = null; }

# Image naming passthrough (parity defaults).
, imageName ? "firestream-odoo"
, imageTag ? odooVersion
}:

let
  # Build-time vendored addons. Built only when specs are present; the resulting
  # derivation lays modules at $out/opt/firestream/odoo/vendor-addons/<module>, which the
  # image builder merges into /opt/firestream/odoo/vendor-addons in the rootfs.
  vendoredAddonsDrv =
    import ./vendor-addons.nix { inherit pkgs lib; } {
      version = odooVersion;
      specs = vendoredAddons;
    };

  # Ordered addon layers, one output directory each at
  # $out/opt/firestream/odoo/addons.d/<NN>-<name>/<module>. Built alongside (not
  # instead of) the legacy derivation above; the legacy output joins this
  # builder's collision check as the implicit lowest-precedence pseudo-layer
  # `legacy-vendor-addons`.
  addonLayersDrv =
    import ./addon-layers.nix { inherit pkgs lib; } {
      version = odooVersion;
      layers = addonLayers;
      legacyDrv = if vendoredAddons != [ ] then vendoredAddonsDrv else null;
    };

  # THE addons_path. Rendered by ./addons-layout.nix — the ONE definition, also
  # consumed by options.nix (which bakes it as ODOO_ADDONS_PATH) and by
  # scripts/config.sh (which reads that env var). Prefer the baked env value
  # when present so an explicit consumer override of ODOO_ADDONS_PATH stays
  # authoritative for BOTH the template and the environment; fall back to
  # recomputing for the legacy direct-import path, which passes plain envVars
  # literals.
  addonsLayout = import ./addons-layout.nix { inherit lib; };
  addonsPath = envVars.ODOO_ADDONS_PATH or (addonsLayout.mkAddonsPath {
    layerDirs = addonsLayout.layerDirNames addonLayers;
    inherit addonsDirPrecedence;
  });

  # Read external script files
  validateScript = builtins.readFile ./scripts/validate.sh;
  configScript = builtins.readFile ./scripts/config.sh;
  initScript = builtins.readFile ./scripts/init.sh;
  secretsScript = builtins.readFile ./scripts/secrets.sh;

  # Odoo-specific helper functions (needed by scripts)
  odooHelpers = ''
    ########################
    # Print validation error
    # Arguments:
    #   $1 - error message
    # Returns:
    #   Sets error_code=1
    #########################
    print_validation_error() {
      error "$1"
      error_code=1
    }

    ########################
    # Check if value is empty
    # Arguments:
    #   $1 - value
    # Returns:
    #   0 if empty, 1 otherwise
    #########################
    is_empty_value() {
      local value="''${1:-}"
      [[ -z "$value" ]]
    }

    ########################
    # Check if value is boolean yes
    # Arguments:
    #   $1 - value
    # Returns:
    #   0 if yes/true/1, 1 otherwise
    #########################
    is_boolean_yes() {
      local bool="''${1:-}"
      [[ "$bool" =~ ^(yes|true|1)$ ]]
    }

    ########################
    # Ensure directory exists
    # Arguments:
    #   $1 - directory path
    #########################
    ensure_dir_exists() {
      local dir="$1"
      [[ -d "$dir" ]] || mkdir -p "$dir"
    }

    ########################
    # Wait for PostgreSQL to be available
    # Uses environment variables for connection details
    # Returns:
    #   0 if connection succeeded, 1 otherwise
    #########################
    odoo_wait_for_postgresql() {
      local host="''${ODOO_DATABASE_HOST:-postgresql}"
      local port="''${ODOO_DATABASE_PORT_NUMBER:-5432}"
      local timeout="''${ODOO_DB_WAIT_TIMEOUT:-120}"

      info "Waiting for PostgreSQL at $host:$port (timeout: $timeout seconds)..."

      if wait-for-port --host "$host" --timeout "$timeout" "$port"; then
        info "PostgreSQL is available at $host:$port"
        return 0
      else
        error "Timeout waiting for PostgreSQL at $host:$port"
        return 1
      fi
    }

    ########################
    # Execute SQL against the Odoo database
    # Arguments:
    #   $1 - SQL statement
    # Returns:
    #   Query result
    #########################
    odoo_db_execute() {
      local sql="''${1:?SQL statement required}"
      local host="''${ODOO_DATABASE_HOST:-postgresql}"
      local port="''${ODOO_DATABASE_PORT_NUMBER:-5432}"
      local db="''${ODOO_DATABASE_NAME:-firestream_odoo}"
      local user="''${ODOO_DATABASE_USER:-firestream}"

      PGPASSWORD="''${ODOO_DATABASE_PASSWORD:-}" ${pkgs.postgresql}/bin/psql \
        -h "$host" \
        -p "$port" \
        -U "$user" \
        -d "$db" \
        -t -c "$sql"
    }

    ########################
    # Execute Odoo CLI command
    # Runs odoo-bin with the current configuration
    # Arguments:
    #   $@ - Arguments to pass to odoo-bin
    #########################
    odoo_execute() {
      local config="''${ODOO_CONF_FILE:-/opt/firestream/odoo/conf/odoo.conf}"

      debug "Executing: python /opt/firestream/odoo/odoo-bin --config=$config --logfile= --pidfile= --stop-after-init $*"
      python /opt/firestream/odoo/odoo-bin \
        --config="$config" \
        --logfile= \
        --pidfile= \
        --stop-after-init \
        "$@"
    }

    ########################
    # Set a configuration value in odoo.conf
    # Arguments:
    #   $1 - key
    #   $2 - value
    #########################
    odoo_conf_set() {
      local key="''${1:?key required}"
      local value="''${2:?value required}"
      local conf_file="''${ODOO_CONF_FILE:-/opt/firestream/odoo/conf/odoo.conf}"

      debug "Setting $key = $value in $conf_file"

      # Escape special characters for sed
      local escaped_key escaped_value
      escaped_key=$(printf '%s\n' "$key" | ${pkgs.gnused}/bin/sed 's/[[\.*^$()+?{|]/\\&/g')
      escaped_value=$(printf '%s\n' "$value" | ${pkgs.gnused}/bin/sed 's/[[\.*^$()+?{|]/\\&/g')

      if grep -q "^[[:space:]]*;*[[:space:]]*$escaped_key[[:space:]]*=" "$conf_file" 2>/dev/null; then
        # Key exists, replace it
        ${pkgs.gnused}/bin/sed -i "s|^[[:space:]]*;*[[:space:]]*$escaped_key[[:space:]]*=.*|$key = $value|" "$conf_file"
      else
        # Key doesn't exist, append it
        echo "$key = $value" >> "$conf_file"
      fi
    }

    ########################
    # Get a configuration value from odoo.conf
    # Arguments:
    #   $1 - key
    # Returns:
    #   Configuration value
    #########################
    odoo_conf_get() {
      local key="''${1:?key required}"
      local conf_file="''${ODOO_CONF_FILE:-/opt/firestream/odoo/conf/odoo.conf}"

      grep "^[[:space:]]*$key[[:space:]]*=" "$conf_file" 2>/dev/null | \
        ${pkgs.gnused}/bin/sed "s|^[[:space:]]*$key[[:space:]]*=[[:space:]]*||" | \
        tr -d "\"' "
    }

    ########################
    # Get Odoo major version
    # Returns:
    #   Major version number (e.g., 18)
    #########################
    odoo_major_version() {
      python /opt/firestream/odoo/odoo-bin --version 2>/dev/null | \
        grep -E -o "[0-9]+\.[0-9]+\.[0-9]+" | \
        cut -d'.' -f1
    }
  '';

  # System dependencies (libs needed in the container)
  systemDeps = with pkgs; [
    # SSL/TLS and crypto
    cacert openssl

    # LDAP
    openldap cyrus_sasl

    # Database clients
    postgresql

    # Compression
    bzip2 xz zlib zstd

    # Version control
    git openssh

    # Process management
    procps

    # Utilities
    curl wget netcat-gnu

    # Terminal
    ncurses readline

    # Image processing (for Pillow and PDF generation)
    imagemagick
    libjpeg
    libpng
    freetype
    fontconfig

    # XML processing
    libxml2
    libxslt

    # PDF generation
    wkhtmltopdf

    # Node.js tools for Odoo assets
    nodejs_22
    nodePackages.rtlcss
    nodePackages.less

    # Fonts (required for proper PDF rendering)
    dejavu_fonts
    liberation_ttf
    noto-fonts
    freefont_ttf

    # C++ runtime
    stdenv.cc.cc.lib
  ];

  # Runtime binary deps (need to be in PATH)
  runtimeBinDeps = with pkgs; [
    coreutils bash gnused gnugrep gawk findutils which
    postgresql git curl netcat-gnu
    wkhtmltopdf
    nodejs_22
    nodePackages.rtlcss
    nodePackages.less
    fontconfig
    firestream.waitForPortPkg  # Required by init scripts for database readiness checks
  ];

  # Odoo major version as an integer, e.g. "18.0" -> 18. Every version dir under
  # ./{15,16,17,18}/module.nix is a thin `args: import ../module.nix args`, so
  # odooVersion is the only version signal this module gets.
  odooMajor = lib.toInt (lib.versions.major odooVersion);

  # The conf key naming the websocket/longpolling port was renamed in Odoo 16.
  # Odoo <= 15 reads `longpolling_port`; >= 16 reads `gevent_port`. Both default
  # to 8072, which is why emitting only `gevent_port` appeared to work on 15 --
  # the key was ignored and the built-in default happened to match. Now that the
  # chart injects ODOO_LONGPOLLING_PORT_NUMBER from `containerPorts.gevent`, that
  # coincidence no longer holds and a non-default port would be silently dropped
  # on 15.
  geventPortKey = if odooMajor >= 16 then "gevent_port" else "longpolling_port";

  # Odoo config template with {{PLACEHOLDER}} syntax
  odooConfigTemplate = ''
    [options]
    ; Addons paths. Computed ONCE by ./addons-layout.nix and shared with the
    ; baked ODOO_ADDONS_PATH env var and scripts/config.sh's fallback generator;
    ; do not hardcode a copy here. /opt/firestream/odoo/vendor-addons is the
    ; baked read-only directory populated from config.odoo.vendoredAddons, and
    ; /opt/firestream/odoo/addons.d/<NN>-<name> are the ordered
    ; config.odoo.addonLayers directories (both empty otherwise).
    addons_path = ${addonsPath}

    ; Admin password for database management
    admin_passwd = {{ODOO_PASSWORD}}

    ; Data directory
    data_dir = {{ODOO_DATA_DIR}}

    ; Log file
    logfile = {{ODOO_LOG_FILE}}

    ; Database connection
    db_host = {{ODOO_DATABASE_HOST}}
    db_name = {{ODOO_DATABASE_NAME}}
    db_password = {{ODOO_DATABASE_PASSWORD}}
    db_port = {{ODOO_DATABASE_PORT_NUMBER}}
    db_user = {{ODOO_DATABASE_USER}}

    ; HTTP configuration
    http_port = {{ODOO_PORT_NUMBER}}
    ${geventPortKey} = {{ODOO_LONGPOLLING_PORT_NUMBER}}

    ; HTTP worker processes. MUST be > 0 for gevent_port above to be bound at
    ; all: Odoo only spawns the gevent (websocket/longpolling) worker in prefork
    ; mode. With workers = 0 Odoo runs threaded and serves websockets on
    ; http_port instead, so a proxy pointing /websocket at gevent_port gets
    ; connection-refused.
    workers = {{ODOO_WORKERS}}

    ; Performance
    limit_time_cpu = 90
    limit_time_real = 150
    max_cron_threads = 1

    ; Security
    list_db = {{ODOO_LIST_DB}}
    proxy_mode = True

    ; Logging
    log_level = {{ODOO_LOG_LEVEL}}
  '';

in firestream.mkPythonContainerModule {
  name = "odoo";
  version = odooVersion;
  inherit pythonEnv python;

  # Paths, environment variables, and secret-aware variables are externalized
  # as function arguments (defaults equal to the historical literals). The
  # legacy flake.nix path uses the defaults; evalContainer passes the same
  # values from options.nix, yielding identical factory args.
  inherit paths envVarsWithSecrets;

  # ODOO_ADDONS_PATH is injected here rather than restated in the envVars
  # default so that BOTH the options.nix path and the legacy direct-import path
  # ship it. `//` with the computed value on the left means a caller-supplied
  # ODOO_ADDONS_PATH still wins (and `addonsPath` above already resolves to that
  # same caller value, keeping the template in step).
  envVars = { ODOO_ADDONS_PATH = addonsPath; } // envVars;

  # Image naming passthrough.
  inherit imageName imageTag;

  # Runtime directories with declarative schema
  runtimeDirs = {
    home = {
      path = "/opt/firestream/odoo";
      type = "conf";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Odoo home directory";
    };
    conf = {
      path = "/opt/firestream/odoo/conf";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Odoo configuration directory";
    };
    data = {
      path = "/firestream/odoo/data";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Odoo data/filestore directory";
    };
    addons = {
      path = "/opt/firestream/odoo/addons";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Custom Odoo addons/modules";
    };
    # Baked read-only addons vendored at build time (config.odoo.vendoredAddons).
    # Declared ephemeral so the directory always exists (and addons_path stays
    # valid) even when no addons are vendored. NOT in ODOO_DATA_TO_PERSIST — it is
    # immutable image content, not PVC state, so the chart never remaps it.
    vendorAddons = {
      path = "/opt/firestream/odoo/vendor-addons";
      type = "data";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Build-time vendored Odoo addons (read-only baseline)";
    };
    # PARENT of the ordered addon-layer directories baked from
    # config.odoo.addonLayers (/opt/firestream/odoo/addons.d/<NN>-<name>). ONE
    # declaration for the parent, not one per layer: the per-layer dirs are
    # image content produced by ./addon-layers.nix, and declaring the parent
    # ephemeral only guarantees it exists so addons_path stays valid when there
    # are zero layers. Like vendorAddons it is immutable image content, so it is
    # deliberately NOT in ODOO_DATA_TO_PERSIST and the chart never remaps it.
    addonLayersDir = {
      path = "/opt/firestream/odoo/addons.d";
      type = "data";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Build-time ordered Odoo addon layers (read-only baseline)";
    };
    logs = {
      path = "/opt/firestream/odoo/log";
      type = "logs";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Odoo log files";
    };
    tmp = {
      path = "/opt/firestream/odoo/tmp";
      type = "tmp";
      persistence = "ephemeral";
      mode = "1777";
      description = "Temporary files";
    };
    volume = {
      path = "/firestream/odoo";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Odoo volume mount point";
    };
    bitnamiPython = {
      path = "/bitnami/python";
      type = "custom";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Custom Python requirements directory";
    };
    state = {
      path = "/firestream/odoo/.state";
      type = "state";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Application state tracking";
    };
  };

  # Build-time: Static config templates
  prepopulateFiles = {
    "/opt/firestream/odoo/conf/odoo.conf.template" = odooConfigTemplate;
  };

  # Per-container helpers: emitted at top-level of libhelpersodoo.sh by the
  # engine, so chart init containers can source the helper from
  # /opt/firestream/scripts/libodoo.sh and use these helpers directly.
  perContainerHelpers = odooHelpers;

  # Validation function
  validateFn = ''
    error_code=0
    ${validateScript}
    [[ "$error_code" -eq 0 ]] || exit "$error_code"
  '';

  # Activation: Load secrets and process config templates
  activateFn = ''
    info "Activating Odoo configuration..."

    # Load secrets from _FILE variables
    ${secretsScript}

    # Compute list_db and log_level values
    local list_db_val log_level_val
    list_db_val="$(is_boolean_yes "$ODOO_LIST_DB" && echo 'True' || echo 'False')"
    log_level_val="$(is_boolean_yes "$BITNAMI_DEBUG" && echo 'debug' || echo 'info')"

    # Process config template if exists. Look in baked /opt/firestream/odoo/conf
    # (where prepopulateFiles puts the template) BEFORE the K8s-overridden
    # ODOO_CONF_DIR — the template is baked into the image, not under PVC.
    local conf_dir="''${ODOO_CONF_DIR:-/opt/firestream/odoo/conf}"
    local conf_file="''${ODOO_CONF_FILE:-$conf_dir/odoo.conf}"
    local template_file="$conf_dir/odoo.conf.template"
    [[ -f "$template_file" ]] || template_file="/opt/firestream/odoo/conf/odoo.conf.template"

    # Ensure conf_dir exists. The conf dir lives under the app tree
    # (/opt/firestream/odoo/conf) on the writable rootfs, not on the data PVC
    # (/firestream/odoo); mkdir -p is a no-op if it already exists.
    ${pkgs.coreutils}/bin/mkdir -p "$conf_dir" 2>/dev/null || true

    if [[ -f "$template_file" ]] && [[ ! -f "$conf_file" ]]; then
      info "Generating odoo.conf from template..."
      ${pkgs.gnused}/bin/sed \
        -e "s|{{ODOO_ADDONS_DIR}}|''${ODOO_ADDONS_DIR:-/opt/firestream/odoo/addons}|g" \
        -e "s|{{ODOO_PASSWORD}}|''${ODOO_PASSWORD:-admin}|g" \
        -e "s|{{ODOO_DATA_DIR}}|''${ODOO_DATA_DIR:-/firestream/odoo/data}|g" \
        -e "s|{{ODOO_LOG_FILE}}|''${ODOO_LOG_FILE:-/opt/firestream/odoo/log/odoo-server.log}|g" \
        -e "s|{{ODOO_DATABASE_HOST}}|''${ODOO_DATABASE_HOST:-postgresql}|g" \
        -e "s|{{ODOO_DATABASE_NAME}}|''${ODOO_DATABASE_NAME:-firestream_odoo}|g" \
        -e "s|{{ODOO_DATABASE_PASSWORD}}|''${ODOO_DATABASE_PASSWORD:-}|g" \
        -e "s|{{ODOO_DATABASE_PORT_NUMBER}}|''${ODOO_DATABASE_PORT_NUMBER:-5432}|g" \
        -e "s|{{ODOO_DATABASE_USER}}|''${ODOO_DATABASE_USER:-firestream}|g" \
        -e "s|{{ODOO_PORT_NUMBER}}|''${ODOO_PORT_NUMBER:-8069}|g" \
        -e "s|{{ODOO_LONGPOLLING_PORT_NUMBER}}|''${ODOO_LONGPOLLING_PORT_NUMBER:-8072}|g" \
        -e "s|{{ODOO_WORKERS}}|''${ODOO_WORKERS:-0}|g" \
        -e "s|{{ODOO_LIST_DB}}|''${list_db_val}|g" \
        -e "s|{{ODOO_LOG_LEVEL}}|''${log_level_val}|g" \
        "$template_file" > "$conf_file"

      info "Generated odoo.conf"
    fi

    # Save config hash for change detection
    save_config_hash "odoo" "$conf_file"

    info "Odoo configuration activated"
  '';

  # Configuration generation
  configFn = configScript;

  # Initialization
  initFn = initScript;

  # Startup command
  runCmd = ''
    ${odooHelpers}

    info "Starting Odoo ${odooVersion}..."

    conf_file="''${ODOO_CONF_FILE:-/opt/firestream/odoo/conf/odoo.conf}"
    pid_file="''${ODOO_PID_FILE:-/opt/firestream/odoo/tmp/odoo.pid}"

    # Run Odoo
    exec python /opt/firestream/odoo/odoo-bin \
      --config="$conf_file" \
      --pidfile="$pid_file"
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;
  inherit health;
  volumes = [ "/firestream/odoo/data" "/opt/firestream/odoo/addons" "/bitnami/python" "/docker-entrypoint-init.d" ];

  user = {
    name = "odoo";
    group = "odoo";
    uid = 1001;
    gid = 1001;
  };

  # Python-specific options
  compileByteCode = false;
  requirementsPath = "/bitnami/python/requirements.txt";
  enablePip = true;

  # Extra packages for the container. Each addons derivation is appended only
  # when its list is non-empty, so the stock image's closure is byte-identical
  # to the pre-vendoring / pre-layers image.
  extraDeps = [ odooSource ]
    ++ lib.optional (vendoredAddons != [ ]) vendoredAddonsDrv
    ++ lib.optional (addonLayers != [ ]) addonLayersDrv;

  # Development shell extras
  devShellPackages = with pkgs; [ uv docker docker-compose ];
  devShellHook = ''
    echo "Odoo Version: ${odooVersion}"
    echo ""
    echo "Build commands:"
    echo "  nix build .#dockerImage    - Build the Docker image"
    echo "  docker load < result       - Load image into Docker"
  '';
}
