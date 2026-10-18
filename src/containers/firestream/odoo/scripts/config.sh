# Odoo configuration generation
# Copyright Firestream. MIT License.
# This file is sourced, not executed directly
#
# Generates odoo.conf from template or defaults

info "Generating Odoo configuration..."

# Ensure config directory exists
ensure_dir_exists "$ODOO_CONF_DIR"

# Compute values
list_db_val="$(is_boolean_yes "$ODOO_LIST_DB" && echo 'True' || echo 'False')"
log_level_val="$(is_boolean_yes "$BITNAMI_DEBUG" && echo 'debug' || echo 'info')"

# The conf key naming the websocket/longpolling port was renamed in Odoo 16:
# <= 15 reads `longpolling_port`, >= 16 reads `gevent_port`. Kept in step with
# `geventPortKey` in ../module.nix.
gevent_port_key="gevent_port"
if [[ "${ODOO_VERSION%%.*}" =~ ^[0-9]+$ ]] && [[ "${ODOO_VERSION%%.*}" -le 15 ]]; then
    gevent_port_key="longpolling_port"
fi

# Generate configuration file if it doesn't exist or if force overwrite is set
if [[ ! -f "$ODOO_CONF_FILE" ]] || is_boolean_yes "${ODOO_FORCE_OVERWRITE_CONF:-no}"; then

    # Check for template file first.
    #
    # NOTE: this is the SECOND conf generator in the image. The primary one is
    # `activateFn` in ../module.nix, which the entrypoint runs before this
    # function (bin/nix/firestream/apps/base.nix: activate at step 5a, configure
    # at step 7), so in the normal container start this branch is a no-op --
    # the conf already exists. It is reachable in two ways, which is why it has
    # to be correct rather than merely dead:
    #   * ODOO_FORCE_OVERWRITE_CONF=yes, settable by a chart consumer through
    #     `extraEnvVars`;
    #   * the `odoo-setup` / `odoo-run` helper scripts, which call
    #     <name>_configure WITHOUT ever calling <name>_activate
    #     (bin/nix/firestream/apps/base.nix, setupScript and runScript).
    #
    # The substitution list below MUST stay in step with the sed pipeline in
    # module.nix's activateFn; both consume the same {{PLACEHOLDER}} template.
    if [[ -f "${ODOO_CONF_FILE}.template" ]]; then
        debug "Generating config from template"
        sed \
            -e "s|{{ODOO_ADDONS_DIR}}|${ODOO_ADDONS_DIR:-/opt/firestream/odoo/addons}|g" \
            -e "s|{{ODOO_PASSWORD}}|${ODOO_PASSWORD:-admin}|g" \
            -e "s|{{ODOO_DATA_DIR}}|${ODOO_DATA_DIR:-/firestream/odoo/data}|g" \
            -e "s|{{ODOO_LOG_FILE}}|${ODOO_LOG_FILE:-/opt/firestream/odoo/log/odoo-server.log}|g" \
            -e "s|{{ODOO_DATABASE_HOST}}|${ODOO_DATABASE_HOST:-postgresql}|g" \
            -e "s|{{ODOO_DATABASE_NAME}}|${ODOO_DATABASE_NAME:-firestream_odoo}|g" \
            -e "s|{{ODOO_DATABASE_PASSWORD}}|${ODOO_DATABASE_PASSWORD:-}|g" \
            -e "s|{{ODOO_DATABASE_PORT_NUMBER}}|${ODOO_DATABASE_PORT_NUMBER:-5432}|g" \
            -e "s|{{ODOO_DATABASE_USER}}|${ODOO_DATABASE_USER:-firestream}|g" \
            -e "s|{{ODOO_PORT_NUMBER}}|${ODOO_PORT_NUMBER:-8069}|g" \
            -e "s|{{ODOO_LONGPOLLING_PORT_NUMBER}}|${ODOO_LONGPOLLING_PORT_NUMBER:-8072}|g" \
            -e "s|{{ODOO_WORKERS}}|${ODOO_WORKERS:-0}|g" \
            -e "s|{{ODOO_LIST_DB}}|${list_db_val}|g" \
            -e "s|{{ODOO_LOG_LEVEL}}|${log_level_val}|g" \
            "${ODOO_CONF_FILE}.template" > "$ODOO_CONF_FILE"
    else
        debug "Generating default config"
        cat > "$ODOO_CONF_FILE" <<EOF
[options]
; Addons paths (Odoo built-in + baked vendored + custom)
addons_path = ${ODOO_BASE_DIR}/addons,${ODOO_BASE_DIR}/odoo/addons,${ODOO_BASE_DIR}/vendor-addons,${ODOO_ADDONS_DIR}

; Admin password for database management (master password)
admin_passwd = ${ODOO_PASSWORD}

; Data directory for filestore
data_dir = ${ODOO_DATA_DIR}

; Log file
logfile = ${ODOO_LOG_FILE}

; Database connection
db_host = ${ODOO_DATABASE_HOST}
db_name = ${ODOO_DATABASE_NAME}
db_password = ${ODOO_DATABASE_PASSWORD}
db_port = ${ODOO_DATABASE_PORT_NUMBER}
db_user = ${ODOO_DATABASE_USER}

; HTTP ports
http_port = ${ODOO_PORT_NUMBER}
${gevent_port_key} = ${ODOO_LONGPOLLING_PORT_NUMBER}

; HTTP worker processes. MUST be > 0 for gevent_port above to be bound at all:
; Odoo only spawns the gevent (websocket/longpolling) worker in prefork mode.
; With workers = 0 Odoo runs threaded and serves websockets on http_port
; instead, so a proxy pointing /websocket at gevent_port gets connection-refused.
; Omitting this line entirely (as this fallback used to) silently forced
; threaded mode no matter what ODOO_WORKERS said.
workers = ${ODOO_WORKERS:-0}

; Performance settings
limit_time_cpu = 90
limit_time_real = 150
max_cron_threads = 1

; Security
list_db = ${list_db_val}
proxy_mode = True

; Debug
log_level = ${log_level_val}
EOF
    fi

    info "Configuration file created: $ODOO_CONF_FILE"
else
    info "Using existing configuration file: $ODOO_CONF_FILE"
fi

# Configure SMTP if specified
if [[ -n "${ODOO_SMTP_HOST:-}" ]]; then
    info "Configuring SMTP settings..."
    odoo_conf_set "smtp_server" "$ODOO_SMTP_HOST"
    odoo_conf_set "smtp_port" "$ODOO_SMTP_PORT_NUMBER"

    if [[ "${ODOO_SMTP_PROTOCOL:-}" == "ssl" ]] || [[ "${ODOO_SMTP_PROTOCOL:-}" == "tls" ]]; then
        odoo_conf_set "smtp_ssl" "True"
    fi

    if [[ -n "${ODOO_SMTP_USER:-}" ]]; then
        odoo_conf_set "smtp_user" "$ODOO_SMTP_USER"
    fi
    if [[ -n "${ODOO_SMTP_PASSWORD:-}" ]]; then
        odoo_conf_set "smtp_password" "$ODOO_SMTP_PASSWORD"
    fi
fi

# Configure database filter if specified
if [[ -n "${ODOO_DATABASE_FILTER:-}" ]]; then
    odoo_conf_set "dbfilter" "$ODOO_DATABASE_FILTER"
fi

# Save config hash for change detection
save_config_hash "odoo" "$ODOO_CONF_FILE"

info "Odoo configuration complete"
