{ pkgs, firestream }:

# End-to-end integration test: builds a sample application using the Firestream
# module system, exercising several modules together.
# Copyright Firestream. MIT License.
#
# NOTE on scope: this scenario previously called `persist_dir`, `persist_file`,
# `generate_start_command`, `backup_persisted_data`, `yml_key_set` and
# `is_dir_persisted` — none of which exist anywhere in the tree, and none of
# which had a caller. Those steps were removed rather than implemented; see
# IMPLEMENTATION_STATUS.md. It also called the real `persist_app` /
# `is_app_initialized` with no arguments, relying on ambient $APP_NAME and
# $PERSISTENCE_ROOT that those functions never read.
#
# Fixtures use printf rather than indented heredocs: an un-dedented heredoc in
# this Nix string leaves two leading spaces on every line, which breaks both
# INI parsing and exact-match assertions.

pkgs.runCommand "test-integration" {} ''
  export HOME=$TMPDIR

  cat > $TMPDIR/test.sh << 'SCRIPT'
  # Import all modules used by the scenario.
  ${firestream.lib.log.functions}
  ${firestream.lib.validations.functions}
  ${firestream.lib.fs.functions}
  ${firestream.lib.os.functions}
  ${firestream.lib.net.functions}
  ${firestream.lib.service.functions}
  ${firestream.lib.file.functions}
  ${firestream.lib.config.functions}
  ${firestream.lib.persistence.functions}

  info "Starting integration test..."

  # ---- 1. Application directory structure ----
  export APP_NAME="myapp"
  export APP_VERSION="1.0.0"
  export APP_HOME="$TMPDIR/myapp"
  export APP_CONF_DIR="$APP_HOME/conf"
  export APP_DATA_DIR="$APP_HOME/data"
  export APP_LOG_DIR="$APP_HOME/logs"

  info "Creating application directories..."
  ensure_dir_exists "$APP_HOME"
  ensure_dir_exists "$APP_CONF_DIR"
  ensure_dir_exists "$APP_DATA_DIR"
  ensure_dir_exists "$APP_LOG_DIR"

  [[ -d "$APP_HOME" ]] || { error "Failed to create APP_HOME"; exit 1; }
  [[ -d "$APP_CONF_DIR" ]] || { error "Failed to create APP_CONF_DIR"; exit 1; }

  # ---- 2. Config file creation + update ----
  info "Creating configuration files..."
  config_file="$APP_CONF_DIR/app.conf"
  printf '[server]\nport = 8080\nhost = localhost\n\n[database]\nurl = postgresql://localhost/mydb\n' > "$config_file"

  ini_file_set "$config_file" "server" "port" "9090"
  [[ "$(ini_get "server" "port" "$config_file")" == "9090" ]] \
    || { error "Config update failed"; exit 1; }
  # The other section must survive the edit.
  [[ "$(ini_get "database" "url" "$config_file")" == "postgresql://localhost/mydb" ]] \
    || { error "ini_file_set disturbed an unrelated section"; exit 1; }

  # ---- 3. Validate configuration values ----
  info "Validating configuration..."
  validate_port 9090 || { error "Port validation failed"; exit 1; }
  ! validate_port 0 2>/dev/null || { error "Port 0 should be rejected"; exit 1; }

  # ---- 4. Persistence round-trip ----
  info "Setting up persistence..."
  persistence_root="$TMPDIR/persistence"
  state_root="$TMPDIR/state"
  ensure_dir_exists "$persistence_root"
  ensure_dir_exists "$state_root"

  echo "payload" > "$APP_DATA_DIR/payload.txt"

  # persist_app <app> <source_dir> <volume_dir>
  persist_app "$APP_NAME" "$APP_DATA_DIR" "$persistence_root" \
    || { error "Failed to persist app"; exit 1; }
  [[ -f "$persistence_root/payload.txt" ]] || { error "Persisted payload missing"; exit 1; }

  mark_app_initialized "$APP_NAME" "$state_root" || { error "Failed to mark initialized"; exit 1; }
  is_app_initialized "$APP_NAME" "$state_root" || { error "App should be initialized"; exit 1; }

  # Wipe and restore.
  rm -rf "$APP_DATA_DIR"
  ensure_dir_exists "$APP_DATA_DIR"
  restore_persisted_app "$APP_NAME" "$APP_DATA_DIR" "$persistence_root" \
    || { error "Failed to restore app"; exit 1; }
  [[ -e "$APP_DATA_DIR/payload.txt" ]] || { error "Restored payload missing"; exit 1; }

  # ---- 5. Service lifecycle against a real process ----
  info "Setting up service..."
  pid_file="$APP_HOME/myapp.pid"
  ${pkgs.coreutils}/bin/sleep 60 &
  echo $! > "$pid_file"
  [[ "$(get_pid_from_file "$pid_file")" == "$(cat "$pid_file")" ]] \
    || { error "get_pid_from_file mismatch"; exit 1; }
  stop_service_using_pid "$pid_file" || { error "Failed to stop service"; exit 1; }

  # ---- 6. Network utilities ----
  info "Testing network utilities..."
  [[ "$(parse_uri "http://localhost:9090/api/v1" scheme)" == "http" ]] || { error "URI parsing failed"; exit 1; }
  [[ "$(parse_uri "http://localhost:9090/api/v1" port)" == "9090" ]] || { error "Port parsing failed"; exit 1; }
  [[ "$(parse_uri "http://localhost:9090/api/v1" path)" == "/api/v1" ]] || { error "Path parsing failed"; exit 1; }

  # ---- 7. File operations ----
  info "Testing file operations..."
  data_file="$APP_DATA_DIR/data.txt"
  echo "initial data" > "$data_file"
  replace_in_file "$data_file" "initial" "updated"
  [[ "$(cat $data_file)" == "updated data" ]] || { error "File replacement failed"; exit 1; }

  # ---- 8. Logging integration ----
  info "Testing logging system..."
  log_file="$APP_LOG_DIR/app.log"
  echo "Application started" > "$log_file"
  echo "Initialization complete" >> "$log_file"
  wait_for_log_entry "Initialization complete" "$log_file" 2 || { error "Log wait failed"; exit 1; }

  # ---- 9. Permission management ----
  info "Setting permissions..."
  configure_permissions_ownership "$APP_HOME" -d "755" -f "644"
  perms=$(${pkgs.coreutils}/bin/stat -c "%a" "$APP_HOME" 2>/dev/null || true)
  [[ "$perms" == "755" ]] || { warn "Unexpected directory permissions: $perms"; }

  # ---- 10. Environment validation ----
  info "Validating environment..."
  export TEST_ENABLED="yes"
  is_boolean_yes "$TEST_ENABLED" || { error "Boolean validation failed"; exit 1; }
  export TEST_PORT="9090"
  is_positive_int "$TEST_PORT" || { error "Integer validation failed"; exit 1; }

  # ---- 11. System information ----
  info "Gathering system information..."
  total_cpus=$(get_total_cpus)
  [[ "$total_cpus" -gt 0 ]] || { error "Failed to get CPU count"; exit 1; }
  info "System has $total_cpus CPUs"

  total_mem=$(get_total_memory)
  [[ "$total_mem" -gt 0 ]] || { error "Failed to get memory"; exit 1; }
  info "System has $total_mem MB memory"

  os_name=$(get_os_metadata --os)
  [[ -n "$os_name" ]] || { error "Failed to get OS metadata"; exit 1; }
  info "OS: $os_name"

  # ---- 12. Cleanup / emptiness predicates ----
  info "Testing cleanup operations..."
  temp_dir="$TMPDIR/temp_test"
  mkdir -p "$temp_dir"
  echo "temp" > "$temp_dir/file.txt"
  ! is_dir_empty "$temp_dir" || { error "Directory should not be empty"; exit 1; }
  rm -rf "$temp_dir"/*
  is_dir_empty "$temp_dir" || { error "Directory should be empty"; exit 1; }

  # ---- 13. Multi-module interaction ----
  info "Testing multi-module interaction..."
  scenario_file="$APP_CONF_DIR/scenario.ini"
  printf '[application]\nname = myapp\nport = 8080\ndebug = false\n' > "$scenario_file"

  port_value=$(ini_get "application" "port" "$scenario_file")
  validate_port "$port_value" || { error "Scenario port validation failed"; exit 1; }

  ini_file_set "$scenario_file" "application" "debug" "true"
  [[ "$(ini_get "application" "debug" "$scenario_file")" == "true" ]] \
    || { error "Scenario debug update failed"; exit 1; }
  info "Updated debug setting in $scenario_file"

  # ---- 14. Final verification ----
  info "Running final verification..."
  [[ -f "$config_file" ]] || { error "Config file missing"; exit 1; }
  [[ -f "$data_file" ]] || { error "Data file missing"; exit 1; }
  [[ -f "$log_file" ]] || { error "Log file missing"; exit 1; }
  [[ -f "$scenario_file" ]] || { error "Scenario file missing"; exit 1; }
  is_app_initialized "$APP_NAME" "$state_root" || { error "App not initialized"; exit 1; }

  info "================================================"
  info "Integration test completed successfully!"
  info "================================================"
  info "Application:    $APP_NAME v$APP_VERSION"
  info "Home:           $APP_HOME"
  info "Config:         $config_file"
  info "Data:           $APP_DATA_DIR"
  info "Logs:           $APP_LOG_DIR"
  info "Persistence:    $persistence_root"
  info "================================================"

  echo "All integration tests passed!"
  SCRIPT

  ${pkgs.bash}/bin/bash $TMPDIR/test.sh
  touch $out
''
