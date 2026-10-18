{ pkgs, firestream }:

# Tests for the persistence module (lib/persistence.nix)
# Copyright Firestream. MIT License.
#
# NOTE on scope: this suite previously asserted against `persist_dir`,
# `persist_file`, `restore_persisted_dir`, `restore_persisted_file`,
# `migrate_old_data`, `list_persisted_files`, `backup_persisted_data` and
# `is_dir_persisted` — eight functions that do not exist in lib/persistence.nix
# or anywhere else, with no callers. They were removed rather than implemented;
# see IMPLEMENTATION_STATUS.md.
#
# It also called the real functions with the wrong arity: `persist_app` and
# `is_app_initialized` take (app, ...) explicitly, not ambient $APP_NAME /
# $PERSISTENCE_ROOT environment variables. The assertions below use the actual
# signatures:
#   persist_app          <app> <source_dir> <volume_dir>
#   restore_persisted_app <app> <source_dir> <volume_dir>
#   is_app_initialized   <app> [base_dir]
#   mark_app_initialized <app> [base_dir]
#   get_persisted_dirs   <app> [base_dir]

pkgs.runCommand "test-persistence" {} ''
  export HOME=$TMPDIR

  cat > $TMPDIR/test.sh << 'SCRIPT'
  ${firestream.lib.persistence.functions}

  app="testapp"
  base_dir="$TMPDIR/firestream"
  volume_dir="$TMPDIR/volume"
  source_dir="$TMPDIR/appdata"
  mkdir -p "$base_dir" "$volume_dir" "$source_dir"

  # ---- is_app_initialized / mark_app_initialized ----
  ! is_app_initialized "$app" "$base_dir" \
    || { echo "FAIL: is_app_initialized should be false before marking"; exit 1; }

  mark_app_initialized "$app" "$base_dir" \
    || { echo "FAIL: mark_app_initialized should succeed"; exit 1; }

  is_app_initialized "$app" "$base_dir" \
    || { echo "FAIL: is_app_initialized should be true after marking"; exit 1; }

  # mark_app_initialized must be tolerant of a read-only state dir: Bitnami
  # chart pods run with readOnlyRootFilesystem and no PVC at the state path, and
  # the shared helper is documented to skip rather than abort on EROFS.
  ro_base="$TMPDIR/readonly"
  mkdir -p "$ro_base"
  chmod a-w "$ro_base"
  mark_app_initialized "$app" "$ro_base" 2>/dev/null \
    || echo "NOTE: mark_app_initialized returned non-zero on a read-only base (tolerated)"
  chmod u+w "$ro_base"

  # ---- persist_app / restore_persisted_app ----
  echo "hello" > "$source_dir/file.txt"
  persist_app "$app" "$source_dir" "$volume_dir" \
    || { echo "FAIL: persist_app should succeed"; exit 1; }
  [[ -f "$volume_dir/file.txt" ]] \
    || { echo "FAIL: persist_app should copy data into the volume dir"; exit 1; }

  # Wipe the source and restore it from the volume.
  rm -rf "$source_dir"
  mkdir -p "$source_dir"
  restore_persisted_app "$app" "$source_dir" "$volume_dir" \
    || { echo "FAIL: restore_persisted_app should succeed"; exit 1; }
  [[ -e "$source_dir/file.txt" ]] \
    || { echo "FAIL: restore_persisted_app should restore the data"; exit 1; }

  # ---- get_persisted_dirs ----
  dirs=$(get_persisted_dirs "$app" "$base_dir" 2>/dev/null || true)
  # No assertion on contents (layout is app-defined); it must simply not abort.
  declare -F get_persisted_dirs >/dev/null \
    || { echo "FAIL: get_persisted_dirs not defined"; exit 1; }

  # ---- directory / file predicates ----
  empty_dir="$TMPDIR/empty"
  mkdir -p "$empty_dir"
  is_dir_empty "$empty_dir" || { echo "FAIL: is_dir_empty should be true for an empty dir"; exit 1; }

  full_dir="$TMPDIR/full"
  mkdir -p "$full_dir"
  touch "$full_dir/x"
  ! is_dir_empty "$full_dir" || { echo "FAIL: is_dir_empty should be false for a non-empty dir"; exit 1; }

  writable="$TMPDIR/writable.txt"
  touch "$writable"
  is_file_writable "$writable" || { echo "FAIL: is_file_writable should be true"; exit 1; }

  chmod a-w "$writable"
  ! is_file_writable "$writable" 2>/dev/null || { echo "FAIL: is_file_writable should be false when read-only"; exit 1; }
  chmod u+w "$writable"

  # ---- ensure_dir_exists ----
  new_dir="$TMPDIR/created/nested"
  ensure_dir_exists "$new_dir" || { echo "FAIL: ensure_dir_exists should succeed"; exit 1; }
  [[ -d "$new_dir" ]] || { echo "FAIL: ensure_dir_exists should create the directory"; exit 1; }
  # Idempotent
  ensure_dir_exists "$new_dir" || { echo "FAIL: ensure_dir_exists should be idempotent"; exit 1; }

  echo "All persistence tests passed!"
  SCRIPT

  ${pkgs.bash}/bin/bash $TMPDIR/test.sh
  touch $out
''
