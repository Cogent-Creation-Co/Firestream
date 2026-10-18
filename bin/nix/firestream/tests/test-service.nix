{ pkgs, firestream }:

# Tests for the service module (lib/service.nix)
# Copyright Firestream. MIT License.
#
# NOTE on scope: this suite previously asserted against six functions that do
# not exist in lib/service.nix, or anywhere else in the tree, and that had no
# callers: `generate_start_command`, `generate_stop_command`,
# `generate_reload_command`, `is_service_enabled`, `wait_for_service` and
# `restart_service_if_needed`. They were removed rather than implemented — see
# IMPLEMENTATION_STATUS.md.
#
# What replaces them are assertions against the functions the module really
# exports: `get_pid_from_file`, `stop_service_using_pid`, `generate_cron_conf`
# and `generate_logrotate_conf`.

pkgs.runCommand "test-service" {} ''
  export HOME=$TMPDIR

  cat > $TMPDIR/test.sh << 'SCRIPT'
  ${firestream.lib.service.functions}

  # Test is_service_running (negative case - no systemd in Nix build)
  ! is_service_running "fake-service" 2>/dev/null || { echo "FAIL: is_service_running should return false"; exit 1; }

  # Test get_pid_from_file — present, well-formed
  pid_file="$TMPDIR/service.pid"
  echo "12345" > "$pid_file"
  pid=$(get_pid_from_file "$pid_file")
  [[ "$pid" == "12345" ]] || { echo "FAIL: get_pid_from_file should read the pid (got '$pid')"; exit 1; }

  # Test get_pid_from_file — missing file must not abort the caller
  missing_pid=$(get_pid_from_file "$TMPDIR/does-not-exist.pid" 2>/dev/null || true)
  [[ -z "$missing_pid" ]] || { echo "FAIL: get_pid_from_file should be empty for a missing file (got '$missing_pid')"; exit 1; }

  # Test get_pid_from_file — non-numeric contents are not a pid
  echo "not-a-pid" > "$pid_file"
  bad_pid=$(get_pid_from_file "$pid_file" 2>/dev/null || true)
  [[ -z "$bad_pid" ]] || { echo "FAIL: get_pid_from_file should reject non-numeric contents (got '$bad_pid')"; exit 1; }

  # Test stop_service_using_pid against a real, short-lived process.
  ${pkgs.coreutils}/bin/sleep 60 &
  real_pid=$!
  echo "$real_pid" > "$pid_file"
  stop_service_using_pid "$pid_file" || { echo "FAIL: stop_service_using_pid should succeed"; exit 1; }
  ${pkgs.coreutils}/bin/sleep 0.3
  ! ${pkgs.coreutils}/bin/kill -0 "$real_pid" 2>/dev/null \
    || { echo "FAIL: stop_service_using_pid should have stopped the process"; exit 1; }

  # Test generate_cron_conf — signature is (output_file) plus flags.
  cron_file="$TMPDIR/cron/testapp"
  mkdir -p "$(dirname "$cron_file")"
  generate_cron_conf "$cron_file" \
    --schedule "*/5 * * * *" \
    --command "echo hello" \
    --run-as "firestream" \
    || { echo "FAIL: generate_cron_conf should succeed"; exit 1; }
  [[ -s "$cron_file" ]] || { echo "FAIL: generate_cron_conf should write $cron_file"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "echo hello" "$cron_file" \
    || { echo "FAIL: cron conf should contain the command"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q '\*/5 \* \* \* \*' "$cron_file" \
    || { echo "FAIL: cron conf should contain the schedule"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "firestream" "$cron_file" \
    || { echo "FAIL: cron conf should contain the run-as user"; exit 1; }

  # Test generate_logrotate_conf — note the argument order is
  # (log_file, output_file), the opposite of generate_cron_conf's.
  logrotate_file="$TMPDIR/logrotate/testapp"
  mkdir -p "$(dirname "$logrotate_file")"
  generate_logrotate_conf "/var/log/testapp/app.log" "$logrotate_file" \
    --period weekly --rotations 7 \
    || { echo "FAIL: generate_logrotate_conf should succeed"; exit 1; }
  [[ -s "$logrotate_file" ]] || { echo "FAIL: generate_logrotate_conf should write $logrotate_file"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "/var/log/testapp" "$logrotate_file" \
    || { echo "FAIL: logrotate conf should contain the log path"; exit 1; }

  echo "All service tests passed!"
  SCRIPT

  ${pkgs.bash}/bin/bash $TMPDIR/test.sh
  touch $out
''
