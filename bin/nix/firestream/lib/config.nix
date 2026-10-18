# config.nix - Configuration file manipulation utilities
# Copyright Firestream. MIT License.
#
# This module provides functions for manipulating configuration files:
# - INI file manipulation (using crudini)
# - Python config file manipulation
# - URL encoding utilities
# - Cryptographic key processing
#
# Usage:
#   ini_set "section" "key" "value" "/path/to/file.ini"
#   python_conf_set "VAR_NAME" "value" "/path/to/config.py"
#   encoded=$(url_encode "user@example.com")

{ pkgs, lib, logModule }:

let
  functions = ''
    # Import dependencies
    ${logModule.functions}

    ########################
    # Set a value in an INI file
    # Arguments:
    #   $1 - section name
    #   $2 - key name
    #   $3 - value
    #   $4 - file path (optional, defaults to $CONFIG_FILE)
    # Returns:
    #   None
    #########################
    ########################
    # Set a value in an INI file, file-first.
    #
    # Thin wrapper over `ini_set` for callers that follow the Bitnami
    # convention of naming the target file first (matching `replace_in_file`,
    # `remove_in_file`, ...). No independent logic — argument order only, so
    # the two can never diverge in behaviour.
    #
    # Arguments:
    #   $1 - file path
    #   $2 - section name
    #   $3 - key name
    #   $4 - value
    # Returns:
    #   None
    #########################
    ini_file_set() {
        local file="''${1:?file is required}"
        local section="''${2:?section is required}"
        local key="''${3:?key is required}"
        local value="''${4-}"

        ini_set "$section" "$key" "$value" "$file"
    }

    ini_set() {
        local section="''${1:?section is required}"
        local key="''${2:?key is required}"
        local value="''${3:-}"
        local file="''${4:-$CONFIG_FILE}"

        if [[ -z "$file" ]]; then
            error "No config file specified"
            return 1
        fi

        debug "Setting [$section] $key = $value in $file"
        ${pkgs.crudini}/bin/crudini --set "$file" "$section" "$key" "$value"
    }

    ########################
    # Get a value from an INI file
    # Arguments:
    #   $1 - section name
    #   $2 - key name
    #   $3 - file path (optional, defaults to $CONFIG_FILE)
    # Returns:
    #   Value or empty string
    #########################
    ini_get() {
        local section="''${1:?section is required}"
        local key="''${2:?key is required}"
        local file="''${3:-$CONFIG_FILE}"

        if [[ -z "$file" ]]; then
            error "No config file specified"
            return 1
        fi

        ${pkgs.crudini}/bin/crudini --get "$file" "$section" "$key" 2>/dev/null || echo ""
    }

    ########################
    # Delete a key from an INI file
    # Arguments:
    #   $1 - section name
    #   $2 - key name
    #   $3 - file path (optional, defaults to $CONFIG_FILE)
    # Returns:
    #   None
    #########################
    ini_del() {
        local section="''${1:?section is required}"
        local key="''${2:?key is required}"
        local file="''${3:-$CONFIG_FILE}"

        if [[ -z "$file" ]]; then
            error "No config file specified"
            return 1
        fi

        debug "Deleting [$section] $key from $file"
        ${pkgs.crudini}/bin/crudini --del "$file" "$section" "$key" 2>/dev/null || true
    }

    ########################
    # Check if a key exists in an INI file
    # Arguments:
    #   $1 - section name
    #   $2 - key name
    #   $3 - file path (optional, defaults to $CONFIG_FILE)
    # Returns:
    #   Boolean
    #########################
    ini_has_key() {
        local section="''${1:?section is required}"
        local key="''${2:?key is required}"
        local file="''${3:-$CONFIG_FILE}"

        if [[ -z "$file" ]]; then
            return 1
        fi

        ${pkgs.crudini}/bin/crudini --get "$file" "$section" "$key" &>/dev/null
    }

    ########################
    # Set a value in a Python config file (KEY = value format)
    # Arguments:
    #   $1 - variable name
    #   $2 - value
    #   $3 - file path
    #   $4 - is_literal (yes/no) - if yes, value is not quoted
    # Returns:
    #   None
    #########################
    python_conf_set() {
        local key="''${1:?key is required}"
        local value="''${2:-}"
        local file="''${3:?file is required}"
        local is_literal="''${4:-no}"

        # Quote string values unless literal
        if [[ "$is_literal" != "yes" && "$is_literal" != "true" && "$is_literal" != "1" ]]; then
            value="'$value'"
        fi

        debug "Setting $key = $value in $file"

        # Check if the key already exists (possibly commented)
        if ${pkgs.gnugrep}/bin/grep -E -q "^#*[[:space:]]*''${key}[[:space:]]*=.*$" "$file" 2>/dev/null; then
            # Replace existing (including commented) line
            ${pkgs.gnused}/bin/sed -E -i "s|^#*[[:space:]]*''${key}[[:space:]]*=.*$|''${key} = ''${value}|" "$file"
        else
            # Append new line
            printf '\n%s = %s' "$key" "$value" >> "$file"
        fi
    }

    ########################
    # Get a value from a Python config file
    # Arguments:
    #   $1 - variable name
    #   $2 - file path
    # Returns:
    #   Value or empty string
    #########################
    python_conf_get() {
        local key="''${1:?key is required}"
        local file="''${2:?file is required}"

        ${pkgs.gnugrep}/bin/grep -E "^[[:space:]]*''${key}[[:space:]]*=" "$file" 2>/dev/null | \
            ${pkgs.gnused}/bin/sed -E "s/^[[:space:]]*''${key}[[:space:]]*=[[:space:]]*//" | \
            ${pkgs.gnused}/bin/sed -E "s/[[:space:]]*#.*$//" | \
            ${pkgs.gnused}/bin/sed -E "s/^['\"]|['\"]$//g"
    }

    ########################
    # URL encode a string
    # Arguments:
    #   $1 - string to encode
    # Returns:
    #   URL-encoded string
    #########################
    url_encode() {
        local string="''${1:-}"
        local old_lc_collate="''${LC_COLLATE:-}"
        LC_COLLATE=C

        local length="''${#string}"
        local i c
        for ((i = 0; i < length; i++)); do
            c="''${string:$i:1}"
            case $c in
                [a-zA-Z0-9.~_-])
                    printf '%s' "$c"
                    ;;
                *)
                    printf '%%%02X' "'$c"
                    ;;
            esac
        done

        LC_COLLATE="$old_lc_collate"
    }

    ########################
    # URL decode a string
    # Arguments:
    #   $1 - string to decode
    # Returns:
    #   URL-decoded string
    #########################
    url_decode() {
        local string="''${1:-}"
        # Replace + with space, then decode %XX sequences
        string="''${string//+/ }"
        printf '%b' "''${string//%/\\x}"
    }

    ########################
    # URL encode for Airflow config (doubles % for INI files)
    # Arguments:
    #   $1 - string to encode
    # Returns:
    #   Airflow-safe URL-encoded string
    #########################
    airflow_encode_url() {
        local url_encoded
        url_encoded=$(url_encode "$1")
        # Double the % signs for INI file compatibility
        echo "''${url_encoded//\%/\%\%}"
    }

    ########################
    # Generate a Fernet key from a raw passphrase
    # Arguments:
    #   $1 - raw key (at least 32 chars)
    # Returns:
    #   Base64-encoded Fernet key
    #########################
    generate_fernet_key() {
        local raw_key="''${1:?raw key is required}"

        if [[ ''${#raw_key} -lt 32 ]]; then
            error "Raw key must have at least 32 characters"
            return 1
        fi

        if [[ ''${#raw_key} -gt 32 ]]; then
            warn "Raw key truncated to 32 characters"
        fi

        echo -n "''${raw_key:0:32}" | ${pkgs.coreutils}/bin/base64
    }

    ########################
    # Process Fernet key from raw or encoded form
    # Uses AIRFLOW_RAW_FERNET_KEY or AIRFLOW_FERNET_KEY env vars
    # Sets AIRFLOW_FERNET_KEY if processing succeeds
    # Returns:
    #   0 on success, 1 on error
    #########################
    process_fernet_key() {
        if [[ -n "''${AIRFLOW_RAW_FERNET_KEY:-}" && -z "''${AIRFLOW_FERNET_KEY:-}" ]]; then
            if [[ ''${#AIRFLOW_RAW_FERNET_KEY} -lt 32 ]]; then
                error "AIRFLOW_RAW_FERNET_KEY must have at least 32 characters"
                return 1
            fi
            [[ ''${#AIRFLOW_RAW_FERNET_KEY} -gt 32 ]] && warn "AIRFLOW_RAW_FERNET_KEY truncated to 32 characters"
            AIRFLOW_FERNET_KEY="$(echo -n "''${AIRFLOW_RAW_FERNET_KEY:0:32}" | ${pkgs.coreutils}/bin/base64)"
            export AIRFLOW_FERNET_KEY
        fi
        return 0
    }

    ########################
    # Process a secret key (truncate and base64 encode)
    # Arguments:
    #   $1 - variable name containing the key
    # Side effects:
    #   Modifies and exports the variable
    #########################
    process_secret_key() {
        local key_var="''${1:?key variable name is required}"
        local key_value="''${!key_var:-}"

        if [[ -z "$key_value" ]]; then
            debug "Secret key $key_var is empty, skipping"
            return 0
        fi

        if [[ ''${#key_value} -gt 32 ]]; then
            warn "''${key_var} truncated to 32 characters"
        fi

        printf -v "$key_var" '%s' "$(echo -n "''${key_value:0:32}" | ${pkgs.coreutils}/bin/base64)"
        export "''${key_var}"
    }

    ########################
    # Generate a random secret key
    # Arguments:
    #   $1 - length (default: 32)
    # Returns:
    #   Random alphanumeric string
    #########################
    generate_secret_key() {
        local length="''${1:-32}"
        ${pkgs.coreutils}/bin/head -c 256 /dev/urandom | ${pkgs.coreutils}/bin/tr -dc 'a-zA-Z0-9' | ${pkgs.coreutils}/bin/head -c "$length"
    }

    ########################
    # Merge two INI files (source overrides target)
    # Arguments:
    #   $1 - source file
    #   $2 - target file
    # Returns:
    #   None (modifies target in place)
    #########################
    ini_merge() {
        local source="''${1:?source file is required}"
        local target="''${2:?target file is required}"

        if [[ ! -f "$source" ]]; then
            error "Source file does not exist: $source"
            return 1
        fi

        if [[ ! -f "$target" ]]; then
            ${pkgs.coreutils}/bin/cp "$source" "$target"
            return 0
        fi

        debug "Merging $source into $target"
        ${pkgs.crudini}/bin/crudini --merge "$target" < "$source"
    }

    ########################
    # Replace placeholders in a file
    # Arguments:
    #   $1 - file path
    #   $2+ - KEY=VALUE pairs
    # Placeholders format: {{KEY}}
    #########################
    replace_placeholders() {
        local file="''${1:?file is required}"
        shift

        if [[ ! -f "$file" ]]; then
            error "File does not exist: $file"
            return 1
        fi

        for pair in "$@"; do
            local key="''${pair%%=*}"
            local value="''${pair#*=}"
            debug "Replacing {{$key}} with $value in $file"
            ${pkgs.gnused}/bin/sed -i "s|{{$key}}|$value|g" "$file"
        done
    }

    ########################
    # Load environment variables from a .env file
    # Arguments:
    #   $1 - file path
    # Side effects:
    #   Exports variables from file
    #########################
    load_env_file() {
        local file="''${1:?file is required}"

        if [[ ! -f "$file" ]]; then
            debug "Env file does not exist: $file"
            return 0
        fi

        debug "Loading environment from $file"
        set -a
        # shellcheck disable=SC1090
        source "$file"
        set +a
    }

    ########################
    # Export a variable if not already set
    # Arguments:
    #   $1 - variable name
    #   $2 - default value
    #########################
    export_default() {
        local var="''${1:?variable name is required}"
        local default="''${2:-}"
        local current="''${!var:-}"

        if [[ -z "$current" ]]; then
            debug "Setting default $var=$default"
            export "''${var}=$default"
        fi
    }
  '';
in
{
  meta = {
    name = "libconfig";
    description = "Configuration file manipulation utilities";
    version = "1.0.0";
  };

  imports = [ logModule ];
  runtimeDeps = with pkgs; [
    coreutils
    crudini
    gnugrep
    gnused
  ];
  inherit functions;
  exports = [
    "ini_set"
    "ini_file_set"
    "ini_get"
    "ini_del"
    "ini_has_key"
    "python_conf_set"
    "python_conf_get"
    "url_encode"
    "url_decode"
    "airflow_encode_url"
    "generate_fernet_key"
    "process_fernet_key"
    "process_secret_key"
    "generate_secret_key"
    "ini_merge"
    "replace_placeholders"
    "load_env_file"
    "export_default"
  ];

  script = pkgs.writeTextDir "opt/firestream/scripts/libconfig.sh" ''
    #!/bin/bash
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.
    ${functions}
  '';
}
