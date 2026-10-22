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

odoo_trim() {
    local value="$1"
    value="${value#"${value%%[![:space:]]*}"}"
    printf '%s' "${value%"${value##*[![:space:]]}"}"
}

########################
# Print the state of the Odoo database: `empty` (no Odoo tables),
# `installed` (the base module is installed), or the state of `base`.
# Returns:
#   1 when the database cannot be read
#########################
odoo_database_state() {
    local table state
    table="$(odoo_db_execute "SELECT to_regclass('public.ir_module_module') IS NOT NULL;")" || return 1
    if [[ "${table//[[:space:]]/}" != "t" ]]; then
        echo "empty"
        return 0
    fi
    state="$(odoo_db_execute "SELECT COALESCE((SELECT state FROM ir_module_module WHERE name = 'base'), 'missing');")" || return 1
    odoo_trim "$state"
}

########################
# Print the first-init state that release mode records in the database:
# `running`, `done`, or `none` when the table does not exist.
# Returns:
#   1 when the database cannot be read
#########################
odoo_first_init_state() {
    local table state
    table="$(odoo_db_execute "SELECT to_regclass('public.firestream_odoo_init') IS NOT NULL;")" || return 1
    if [[ "${table//[[:space:]]/}" != "t" ]]; then
        echo "none"
        return 0
    fi
    state="$(odoo_db_execute "SELECT COALESCE((SELECT state FROM firestream_odoo_init WHERE id = 1), 'none');")" || return 1
    odoo_trim "$state"
}

odoo_set_first_init_state() {
    odoo_db_execute "CREATE TABLE IF NOT EXISTS firestream_odoo_init (id integer PRIMARY KEY CHECK (id = 1), state text NOT NULL); INSERT INTO firestream_odoo_init (id, state) VALUES (1, '$1') ON CONFLICT (id) DO UPDATE SET state = EXCLUDED.state;" >/dev/null
}

########################
# Print the modules of ODOO_INSTALL_MODULES that are not installed,
# one per line, and the modules in a partial state as `partial:<name>`.
#########################
odoo_module_gaps() {
    local wanted=""
    if [[ -n "${ODOO_INSTALL_MODULES//[, ]/}" ]]; then
        wanted="$(printf "'%s'," ${ODOO_INSTALL_MODULES//,/ })"
    fi
    odoo_db_execute "SELECT w FROM unnest(ARRAY[${wanted%,}]::text[]) AS w WHERE NOT EXISTS (SELECT 1 FROM ir_module_module m WHERE m.name = w AND m.state = 'installed') UNION ALL SELECT 'partial:' || name FROM ir_module_module WHERE state IN ('to install', 'to upgrade', 'to remove');" \
        | sed -e 's/^[[:space:]]*//' -e '/^$/d'
}

odoo_database_exists=no
odoo_release_first_init=no
if [[ "${ODOO_MODULE_SYNC:-boot}" == "release" ]]; then
    # NOTE: with release sync the pod may have no persistent disk, and then
    # the marker file is gone at each start. The database tells the truth:
    # the table firestream_odoo_init records a first init that has not
    # finished, so the next boot finishes it.
    # WARNING: a read error must stop the boot. Read as "empty", it would
    # run `--init=base` on a live database and reset the admin credentials.
    if ! db_state="$(odoo_database_state)" || ! init_state="$(odoo_first_init_state)"; then
        error "Cannot read the Odoo tables of database '${ODOO_DATABASE_NAME}'"
        exit 1
    fi
    if [[ "$db_state" == "empty" || "$init_state" == "running" ]]; then
        odoo_release_first_init=yes
    elif [[ "$db_state" != "installed" ]] && ! is_boolean_yes "${ODOO_SYNC_ON_BOOT:-no}"; then
        # WARNING: `--init=base` on this database would reinstall base on
        # live data and reset the admin credentials.
        error "The base module of database '${ODOO_DATABASE_NAME}' is in state '${db_state}', not installed."
        error "A module change did not finish. Set ODOO_SYNC_ON_BOOT=yes for one boot, or run firestream-odoo-sync."
        exit 1
    else
        odoo_database_exists=yes
    fi
elif [[ -f "$ODOO_INIT_MARKER" ]]; then
    odoo_database_exists=yes
fi

if [[ "$odoo_database_exists" == "yes" && "${ODOO_MODULE_SYNC:-boot}" == "release" ]]; then
    if is_boolean_yes "${ODOO_SYNC_ON_BOOT:-no}"; then
        info "ODOO_SYNC_ON_BOOT=yes: running firestream-odoo-sync before the server starts..."
        firestream-odoo-sync || { error "firestream-odoo-sync failed"; exit 1; }
    else
        info "Odoo database exists; ODOO_MODULE_SYNC=release, so the boot changes no module."
        info "The release runs firestream-odoo-sync to install and update modules."
    fi
    gaps="$(odoo_module_gaps)" || { error "Cannot read the module states"; exit 1; }
    if grep -q '^partial:' <<< "$gaps"; then
        error "These modules did not finish a change: $(grep '^partial:' <<< "$gaps" | cut -d: -f2 | paste -sd, -)"
        error "Set ODOO_SYNC_ON_BOOT=yes for one boot, or run firestream-odoo-sync."
        exit 1
    fi
    if [[ -n "$gaps" ]]; then
        warn "These install modules are not installed yet: $(paste -sd, - <<< "$gaps")"
        warn "Run firestream-odoo-sync to install them."
    fi
    touch "$ODOO_INIT_MARKER"
elif [[ "$odoo_database_exists" == "yes" ]]; then
    info "Odoo already initialized, restoring persisted state..."

    # Odoo records the demo decision per-database at first init; demo data can
    # never be retrofitted into an existing DB (modules installed later skip
    # their demo data too). Surface the mismatch instead of failing silently.
    if is_boolean_yes "$ODOO_LOAD_DEMO_DATA"; then
        warn "ODOO_LOAD_DEMO_DATA=yes has no effect on an already-initialized database."
        warn "Demo data only loads at first init - drop the database and data volumes to reseed."
    fi

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
        # WARNING: a boot that continues after a failed sync serves the old
        # module set. When the missing module is a security module (for
        # example a sign-in policy), the site then runs without it.
        odoo_execute "${boot_args[@]}" || { error "Module sync failed"; exit 1; }
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

    if [[ "$odoo_release_first_init" == "yes" && "$db_state" != "empty" ]]; then
        # NOTE: an interrupted first init leaves attachment rows whose files
        # were on the disk of the pod that stopped. The server never started
        # on this database (the state is still `running`), so it holds no
        # user data, and a clean install is the only way to whole files.
        warn "An earlier first init of '${ODOO_DATABASE_NAME}' did not finish; installing again from an empty database."
        # NOTE: one DROP per table, each in its own transaction. One
        # `DROP SCHEMA ... CASCADE` on an Odoo schema needs a lock for each
        # of its objects and exceeds max_locks_per_transaction.
        if ! odoo_db_execute "SELECT format('DROP TABLE IF EXISTS public.%I CASCADE;', tablename) FROM pg_tables WHERE schemaname = 'public' AND tablename <> 'firestream_odoo_init';" \
                | odoo_pg psql -q -v ON_ERROR_STOP=1 --no-password >/dev/null \
            || ! odoo_db_execute "DROP SCHEMA public CASCADE; CREATE SCHEMA public;" >/dev/null; then
            error "Cannot empty database '${ODOO_DATABASE_NAME}' for a new first init"
            exit 1
        fi
        rm -rf "${ODOO_DATA_DIR}/filestore/${ODOO_DATABASE_NAME}"
    fi
    if [[ "$odoo_release_first_init" == "yes" ]]; then
        odoo_set_first_init_state running || { error "Cannot record the first init"; exit 1; }
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

        if [[ "$odoo_release_first_init" == "yes" ]]; then
            gaps="$(odoo_module_gaps)" || { error "Cannot read the module states"; exit 1; }
            if [[ -n "$gaps" ]]; then
                error "The first init did not install: $(paste -sd, - <<< "$gaps")"
                exit 1
            fi
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

    if [[ "$odoo_release_first_init" == "yes" ]]; then
        # NOTE: modules that load before a storage module write their
        # attachments to the local filestore. The first-init calls (for
        # example a move into object storage) run before the boot is done,
        # while those files still exist on this pod.
        for odoo_call in ${ODOO_FIRST_INIT_CALLS:-}; do
            info "First init call: ${odoo_call}"
            firestream-odoo-call "${odoo_call%%:*}" "${odoo_call#*:}" \
                || { error "First init call ${odoo_call} failed"; exit 1; }
        done
        # NOTE: a fresh install matches the code of this image, so its
        # checksums are the baseline. Without them the first release sync
        # would update every module.
        firestream-odoo-sync --save-checksums || { error "Cannot save the module checksums"; exit 1; }
        odoo_set_first_init_state done || { error "Cannot record the first init"; exit 1; }
    fi

    # Create initialization marker
    touch "$ODOO_INIT_MARKER"
    info "Initialization marker created"
fi

info "Odoo initialization complete"
