# Odoo initialization logic
# Copyright Firestream. MIT License.
# This file is sourced, not executed directly
#
# Handles database initialization, module installation, and admin user setup

info "Initializing Odoo (version ${ODOO_VERSION:-18})..."

# Create required directories
for dir in "$ODOO_BASE_DIR" "$ODOO_CONF_DIR" "$ODOO_DATA_DIR" \
           "$ODOO_ADDONS_DIR" "$ODOO_LOGS_DIR" "$ODOO_TMP_DIR"; do
    ensure_dir_exists "$dir"
done

# Ensure volume directory exists and has correct permissions
ensure_dir_exists "$ODOO_VOLUME_DIR"

# Wait for PostgreSQL
info "Waiting for PostgreSQL at ${ODOO_DATABASE_HOST}:${ODOO_DATABASE_PORT_NUMBER}..."
if ! odoo_wait_for_postgresql; then
    error "Failed to connect to PostgreSQL"
    exit 1
fi

# Check if this is first run (no initialization marker)
ODOO_INIT_MARKER="${ODOO_DATA_DIR}/.odoo_initialized"

if [[ -f "$ODOO_INIT_MARKER" ]]; then
    info "Odoo already initialized, restoring persisted state..."

    # Ensure baked modules are installed, and optionally update the rest.
    # --init on ODOO_INSTALL_MODULES makes images self-installing on EXISTING
    # databases too (a new module baked into a rebuilt image lands on the next
    # boot): --init is idempotent — missing modules are installed, present ones
    # updated. Both flags share one odoo run to keep boot time down.
    local -a boot_args=()
    if [[ -n "${ODOO_INSTALL_MODULES:-}" ]]; then
        boot_args+=("--init=${ODOO_INSTALL_MODULES}")
    fi
    if ! is_boolean_yes "$ODOO_SKIP_MODULES_UPDATE"; then
        boot_args+=("--update=all")
    fi
    if [[ ${#boot_args[@]} -gt 0 ]]; then
        info "Syncing Odoo modules (${boot_args[*]})..."
        odoo_execute "${boot_args[@]}" || warn "Module sync failed, continuing..."
    fi
else
    info "First run detected - initializing Odoo..."

    # Validate database ownership
    # Odoo requires the database to be owned by the same user
    info "Validating database owner..."
    local db_owner_check
    db_owner_check=$(odoo_db_execute "SELECT u.usename FROM pg_database d JOIN pg_user u ON (d.datdba = u.usesysid) WHERE d.datname = '${ODOO_DATABASE_NAME}' AND u.usename = '${ODOO_DATABASE_USER}';" 2>/dev/null || true)

    if [[ -z "$db_owner_check" ]] || [[ "$db_owner_check" == *"(0 rows)"* ]]; then
        warn "Database '${ODOO_DATABASE_NAME}' may not be owned by '${ODOO_DATABASE_USER}'"
        warn "This could cause Odoo to not recognize the database"
    fi

    if ! is_boolean_yes "$ODOO_SKIP_BOOTSTRAP"; then
        info "Installing Odoo modules..."

        # Build init arguments — base plus any baked auto-install modules
        # (comma-separated ODOO_INSTALL_MODULES, see odoo.installModules).
        local -a init_args=("--init=base${ODOO_INSTALL_MODULES:+,${ODOO_INSTALL_MODULES}}")

        # Add demo data flag
        if ! is_boolean_yes "$ODOO_LOAD_DEMO_DATA"; then
            init_args+=("--without-demo=all")
        fi

        # Run Odoo initialization
        if ! odoo_execute "${init_args[@]}"; then
            error "Failed to initialize Odoo modules"
            exit 1
        fi

        # Update admin user credentials
        info "Updating admin user credentials..."
        odoo_db_execute "UPDATE res_users SET login = '${ODOO_EMAIL}', password = '${ODOO_PASSWORD}' WHERE login = 'admin';" || warn "Failed to update admin credentials"

        info "Odoo modules installed successfully"
    else
        info "ODOO_SKIP_BOOTSTRAP is set - skipping module installation"

        # Clear assets cache if using existing database
        info "Clearing assets cache from database..."
        odoo_db_execute "DELETE FROM ir_attachment WHERE url LIKE '/web/content/%';" || warn "Failed to clear assets cache"

        if ! is_boolean_yes "$ODOO_SKIP_MODULES_UPDATE"; then
            info "Updating modules..."
            odoo_execute --update=all || warn "Module update failed, continuing..."
        fi
    fi

    # Create initialization marker
    touch "$ODOO_INIT_MARKER"
    info "Initialization marker created"
fi

info "Odoo initialization complete"
