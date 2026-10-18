{ pkgs, firestream }:

# Tests for the file module (lib/file.nix)
# Copyright Firestream. MIT License.
#
# NOTE on scope: this suite previously asserted against `yml_key_set`,
# `xml_set`, `json_set` and `append_file_if_not_exists`, none of which exist in
# lib/file.nix (or anywhere else) and none of which had a caller. They were
# removed rather than implemented — see IMPLEMENTATION_STATUS.md. The
# assertions below cover the functions the module actually exports.
#
# `ini_file_set` DOES exist, in lib/config.nix (it is a file-first wrapper over
# `ini_set`), so that module is sourced alongside file.
#
# All fixtures are written with printf rather than an indented heredoc: an
# un-dedented heredoc inside this Nix string leaves two leading spaces on every
# line, which silently broke exact-match assertions like
# `[[ "$result" == "key1=newvalue" ]]`.

pkgs.runCommand "test-file" {} ''
  export HOME=$TMPDIR

  cat > $TMPDIR/test.sh << 'SCRIPT'
  ${firestream.lib.file.functions}
  ${firestream.lib.config.functions}

  # ---- replace_in_file (filename, match, substitution) ----
  # Argument order matches upstream Bitnami and every caller in the tree.
  test_file="$TMPDIR/test.txt"
  echo "Hello PLACEHOLDER world" > "$test_file"
  replace_in_file "$test_file" "PLACEHOLDER" "beautiful"
  result=$(cat "$test_file")
  [[ "$result" == "Hello beautiful world" ]] || { echo "FAIL: replace_in_file basic replacement (got: $result)"; exit 1; }

  # Regex replacement
  echo "version=1.2.3" > "$test_file"
  replace_in_file "$test_file" 'version=[0-9.]*' 'version=2.0.0'
  result=$(cat "$test_file")
  [[ "$result" == "version=2.0.0" ]] || { echo "FAIL: replace_in_file regex (got: $result)"; exit 1; }

  # A substitution containing the historical default delimiter '/' must be safe:
  # the helper uses a non-printable delimiter precisely so this cannot break.
  echo "path = OLD" > "$test_file"
  replace_in_file "$test_file" '^path = .*' 'path = /var/lib/firestream'
  result=$(cat "$test_file")
  [[ "$result" == "path = /var/lib/firestream" ]] || { echo "FAIL: replace_in_file with slashes (got: $result)"; exit 1; }

  # posix_regex=false selects BRE
  echo "aaa" > "$test_file"
  replace_in_file "$test_file" 'a\{3\}' 'bbb' false
  result=$(cat "$test_file")
  [[ "$result" == "bbb" ]] || { echo "FAIL: replace_in_file BRE mode (got: $result)"; exit 1; }

  # ---- replace_in_file_multiline (filename, match, substitution) ----
  ml_file="$TMPDIR/multiline.txt"
  printf 'aaa\nbbb\nccc\n' > "$ml_file"
  replace_in_file_multiline "$ml_file" 'aaa\nbbb' 'XXX'
  ${pkgs.gnugrep}/bin/grep -q "XXX" "$ml_file" || { echo "FAIL: replace_in_file_multiline should substitute across lines"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "ccc" "$ml_file" || { echo "FAIL: replace_in_file_multiline should keep the rest"; exit 1; }

  # ---- remove_in_file (filename, pattern) ----
  remove_file="$TMPDIR/remove.txt"
  printf 'line1\nline2\nline3\n' > "$remove_file"
  remove_in_file "$remove_file" "line2"
  ! ${pkgs.gnugrep}/bin/grep -q "line2" "$remove_file" || { echo "FAIL: remove_in_file should remove line"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "line1" "$remove_file" || { echo "FAIL: remove_in_file should keep other lines"; exit 1; }
  ${pkgs.gnugrep}/bin/grep -q "line3" "$remove_file" || { echo "FAIL: remove_in_file should keep other lines"; exit 1; }

  # ---- append_file_after_last_match ----
  append_target="$TMPDIR/append.conf"
  printf '[main]\nkey=1\n[other]\nkey=2\n' > "$append_target"
  append_file_after_last_match "$append_target" '^key=' 'key=3'
  ${pkgs.gnugrep}/bin/grep -q "key=3" "$append_target" || { echo "FAIL: append_file_after_last_match should insert the line"; exit 1; }

  # ---- ini_file_set (file, section, key, value) — lib/config.nix ----
  ini_file="$TMPDIR/test.ini"
  printf '[section]\nkey1=value1\nkey2=value2\n' > "$ini_file"
  ini_file_set "$ini_file" "section" "key1" "newvalue"
  result=$(${pkgs.gnugrep}/bin/grep "^key1" "$ini_file")
  [[ "$result" == "key1=newvalue" ]] || { echo "FAIL: ini_file_set (got: $result)"; exit 1; }
  # Sibling keys must be untouched.
  ${pkgs.gnugrep}/bin/grep -q "^key2=value2" "$ini_file" || { echo "FAIL: ini_file_set should not disturb other keys"; exit 1; }

  # ---- wait_for_log_entry ----
  log_target="$TMPDIR/wait.log"
  echo "Starting..." > "$log_target"
  ( ${pkgs.coreutils}/bin/sleep 0.2 && echo "SUCCESS_MARKER" >> "$log_target" ) &
  wait_for_log_entry "SUCCESS_MARKER" "$log_target" 5 || { echo "FAIL: wait_for_log_entry should find the marker"; exit 1; }
  ! wait_for_log_entry "NEVER_APPEARS" "$log_target" 1 2>/dev/null || { echo "FAIL: wait_for_log_entry should time out"; exit 1; }

  echo "All file tests passed!"
  SCRIPT

  ${pkgs.bash}/bin/bash $TMPDIR/test.sh
  touch $out
''
