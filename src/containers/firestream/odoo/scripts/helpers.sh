# Odoo dump and restore helpers
# Copyright Firestream. MIT License.
#
# Function definitions only, no side effects. This file is read into the
# `odooHelpers` string in module.nix and emitted at top level of
# /opt/firestream/scripts/libhelpersodoo.sh, so a Job or an interactive shell
# can `source` that file and call the functions directly.
#
# An Odoo backup has three parts, and all three travel in one archive:
#   manifest.json  - database name, Odoo version, installed modules
#   dump.sql       - plain-text `pg_dump --clean --if-exists` of the database
#   filestore/     - the attachment store from $ODOO_DATA_DIR/filestore/<db>
# These are the same members as Odoo's own zip backup, so the archive can be
# repackaged for the web database manager.

########################
# Run a PostgreSQL client with the Odoo database connection in the environment
# Arguments:
#   $@ - command and arguments (psql, pg_dump, ...)
#########################
odoo_pg() {
  local pass="${ODOO_DATABASE_PASSWORD:-}"
  if [[ -z "$pass" && -n "${ODOO_DATABASE_PASSWORD_FILE:-}" && -r "${ODOO_DATABASE_PASSWORD_FILE}" ]]; then
    pass="$(< "${ODOO_DATABASE_PASSWORD_FILE}")"
  fi
  PGHOST="${ODOO_DATABASE_HOST:-postgresql}" \
  PGPORT="${ODOO_DATABASE_PORT_NUMBER:-5432}" \
  PGUSER="${ODOO_DATABASE_USER:-firestream}" \
  PGDATABASE="${ODOO_DATABASE_NAME:-firestream_odoo}" \
  PGPASSWORD="$pass" \
    "$@"
}

########################
# Write the backup manifest
# Arguments:
#   $1 - manifest path
#   $2 - database name
#########################
odoo_write_backup_manifest() {
  local path="${1:?manifest path required}"
  local db="${2:?database name required}"
  local modules pg_version
  modules="$(odoo_pg psql -tA --no-password -c \
    "SELECT COALESCE(json_object_agg(name, latest_version), '{}'::json) FROM ir_module_module WHERE state = 'installed';" 2>/dev/null || echo '{}')"
  pg_version="$(odoo_pg psql -tA --no-password -c "SHOW server_version;" 2>/dev/null || echo 'unknown')"
  cat > "$path" <<EOF
{
  "odoo_dump": "1",
  "db_name": "$db",
  "version": "${ODOO_VERSION:-unknown}",
  "pg_version": "$pg_version",
  "created_at": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "modules": ${modules}
}
EOF
}

########################
# Dump the Odoo database and filestore into one archive
# Arguments:
#   $1 - output directory
# Returns:
#   Prints the archive path on the last line of stdout
#########################
odoo_dump() {
  local out_dir="${1:?output directory required}"
  local db="${ODOO_DATABASE_NAME:-firestream_odoo}"
  local data_dir="${ODOO_DATA_DIR:-/firestream/odoo/data}"
  local ts
  ts="$(date '+%Y-%m-%d-%H-%M-%S')"
  local archive="$out_dir/odoo-$db-$ts.tar.gz"
  local staging
  staging="$(mktemp -d "${TMPDIR:-/tmp}/odoo-dump.XXXXXX")" || return 1
  mkdir -p "$out_dir"

  info "Dumping database $db..."
  if ! odoo_pg pg_dump --format=plain --clean --if-exists --no-owner --no-privileges \
      --no-password --file="$staging/dump.sql"; then
    error "pg_dump of $db failed"
    rm -rf "$staging"
    return 1
  fi

  odoo_write_backup_manifest "$staging/manifest.json" "$db"

  local filestore="$data_dir/filestore/$db"
  if [[ -d "$filestore" ]]; then
    ln -s "$filestore" "$staging/filestore"
  else
    warn "No filestore at $filestore; archiving an empty filestore"
    mkdir -p "$staging/filestore"
  fi

  # NOTE: -h dereferences the filestore symlink so the archive carries a real
  # `filestore/` directory without copying the attachments into staging first.
  if ! tar -C "$staging" -czhf "$archive" manifest.json dump.sql filestore; then
    error "Failed to write $archive"
    rm -rf "$staging"
    return 1
  fi
  rm -rf "$staging"

  info "Backup archive written: $archive"
  echo "$archive"
}

########################
# Restore an Odoo database and filestore from an archive made by odoo_dump.
# Refuses to run while an odoo-bin process is alive unless ODOO_RESTORE_FORCE=yes.
# Arguments:
#   $1 - archive path
#########################
odoo_restore() {
  local archive="${1:?archive path required}"
  local db="${ODOO_DATABASE_NAME:-firestream_odoo}"
  local data_dir="${ODOO_DATA_DIR:-/firestream/odoo/data}"

  if [[ ! -r "$archive" ]]; then
    error "Archive not readable: $archive"
    return 1
  fi
  if pgrep -f 'odoo-bin' >/dev/null 2>&1 && ! is_boolean_yes "${ODOO_RESTORE_FORCE:-no}"; then
    error "Odoo is running. Stop it before a restore, or set ODOO_RESTORE_FORCE=yes."
    return 1
  fi

  local staging
  staging="$(mktemp -d "${TMPDIR:-/tmp}/odoo-restore.XXXXXX")" || return 1
  if ! tar -C "$staging" -xzf "$archive"; then
    error "Failed to unpack $archive"
    rm -rf "$staging"
    return 1
  fi
  if [[ ! -f "$staging/dump.sql" ]]; then
    error "Archive has no dump.sql: $archive"
    rm -rf "$staging"
    return 1
  fi

  if [[ -f "$staging/manifest.json" ]]; then
    local archive_version
    archive_version="$(python -c 'import json,sys; print(json.load(open(sys.argv[1])).get("version", ""))' "$staging/manifest.json" 2>/dev/null || true)"
    if [[ -n "$archive_version" && -n "${ODOO_VERSION:-}" && "${archive_version%%.*}" != "${ODOO_VERSION%%.*}" ]]; then
      if is_boolean_yes "${ODOO_RESTORE_FORCE:-no}"; then
        warn "Archive is Odoo $archive_version, image is Odoo $ODOO_VERSION; continuing because ODOO_RESTORE_FORCE=yes"
      else
        error "Archive is Odoo $archive_version, image is Odoo $ODOO_VERSION. Set ODOO_RESTORE_FORCE=yes to restore anyway."
        rm -rf "$staging"
        return 1
      fi
    fi
  fi

  info "Terminating other connections to $db..."
  odoo_pg psql -q --no-password -c \
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid();" \
    >/dev/null || warn "Could not terminate other connections; continuing"

  # NOTE: the dump was made with --clean --if-exists, so replaying it drops and
  # recreates every object and a second run after a partial failure is safe.
  # psql keeps going past per-statement errors (for example DROP EXTENSION by a
  # non-superuser); only a connection failure returns non-zero here.
  info "Loading database $db..."
  if ! odoo_pg psql -q --no-password --file="$staging/dump.sql" >/dev/null; then
    error "psql could not load $staging/dump.sql into $db"
    rm -rf "$staging"
    return 1
  fi

  info "Restoring filestore..."
  local dest="$data_dir/filestore/$db"
  mkdir -p "$data_dir/filestore"
  rm -rf "$dest"
  if [[ -d "$staging/filestore" ]]; then
    cp -a "$staging/filestore" "$dest"
  else
    mkdir -p "$dest"
  fi
  rm -rf "$data_dir/sessions"

  # WARNING: without this marker init.sh treats the next boot as a first run
  # and executes `--init=base` against the restored database.
  touch "$data_dir/.odoo_initialized"

  rm -rf "$staging"
  info "Restore of $db from $archive complete"
}

########################
# Configure the aws CLI from the S3_* env and wait for the endpoint.
# Uses S3_ENDPOINT_URL, S3_BACKUP_BUCKET, S3_ADDRESSING_STYLE and
# FIRESTREAM_BACKUP_S3_WAIT_ATTEMPTS / FIRESTREAM_BACKUP_S3_WAIT_INTERVAL.
#########################
odoo_s3_wait() {
  export HOME="${HOME:-/tmp}"
  if [[ -n "${S3_ADDRESSING_STYLE:-}" ]]; then
    aws configure set default.s3.addressing_style "$S3_ADDRESSING_STYLE"
  fi
  local waitn="${FIRESTREAM_BACKUP_S3_WAIT_ATTEMPTS:-60}"
  local waiti="${FIRESTREAM_BACKUP_S3_WAIT_INTERVAL:-5}"
  local i=1
  while [[ "$i" -le "$waitn" ]]; do
    if aws s3 ls "s3://$S3_BACKUP_BUCKET/" ${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"} >/dev/null 2>&1; then
      return 0
    fi
    echo "[firestream] waiting for S3 endpoint ($i/$waitn)..." >&2
    i=$((i + 1))
    sleep "$waiti"
  done
  echo "[firestream] S3 endpoint unreachable after $waitn attempts" >&2
  return 1
}

########################
# Dump Odoo and upload the archive to S3.
# Prints `[firestream] backup complete: <s3 key>` on success; the
# `firestream helm backup` CLI scrapes that line.
#########################
odoo_backup_s3() {
  odoo_s3_wait || return 1

  local archive
  archive="$(odoo_dump "${TMPDIR:-/tmp}/odoo-backup" | tail -n 1)" || return 1
  [[ -f "$archive" ]] || { echo "[firestream] no archive produced" >&2; return 1; }

  local key="s3://$S3_BACKUP_BUCKET/$S3_BACKUP_PREFIX/$(basename "$archive")"
  echo "[firestream] uploading to $key (endpoint=${S3_ENDPOINT_URL:-<aws-default>})"
  local max="${FIRESTREAM_BACKUP_MAX_ATTEMPTS:-3}"
  local attempt=1
  while :; do
    if aws s3 cp "$archive" "$key" ${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"}; then
      rm -f "$archive"
      echo "[firestream] backup complete: $key"
      return 0
    fi
    if [[ "$attempt" -ge "$max" ]]; then
      echo "[firestream] backup failed after $max attempts" >&2
      rm -f "$archive"
      return 1
    fi
    echo "[firestream] upload attempt $attempt failed; retrying in 10s..." >&2
    attempt=$((attempt + 1))
    sleep 10
  done
}

########################
# Download an archive from S3 and restore it.
# S3_BACKUP_KEY may be a bucket-relative key or a full s3:// URI.
#########################
odoo_restore_s3() {
  odoo_s3_wait || return 1

  local uri="${S3_BACKUP_KEY:?S3_BACKUP_KEY required}"
  case "$uri" in s3://*) ;; *) uri="s3://$S3_BACKUP_BUCKET/$uri" ;; esac
  local archive="${TMPDIR:-/tmp}/$(basename "$uri")"

  echo "[firestream] downloading $uri"
  local max="${FIRESTREAM_RESTORE_MAX_ATTEMPTS:-3}"
  local attempt=1
  while :; do
    if aws s3 cp "$uri" "$archive" ${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"}; then
      break
    fi
    if [[ "$attempt" -ge "$max" ]]; then
      echo "[firestream] download failed after $max attempts" >&2
      return 1
    fi
    echo "[firestream] download attempt $attempt failed; retrying in 10s..." >&2
    attempt=$((attempt + 1))
    sleep 10
  done

  if odoo_restore "$archive"; then
    rm -f "$archive"
    echo "[firestream] restore complete"
    return 0
  fi
  rm -f "$archive"
  echo "[firestream] restore failed" >&2
  return 1
}
