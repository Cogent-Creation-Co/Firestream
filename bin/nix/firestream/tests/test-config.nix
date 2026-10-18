# Tests for the config module (lib/config.nix)
# Copyright Firestream. MIT License.
{ pkgs, firestream }:

let
  configModule = firestream.lib.config;

  # Test that all expected functions are present.
  #
  # This inspects the emitted library FILE, not a shell string. The previous
  # version did `functions="${"\${"}configModule.functions}"` — interpolating the
  # entire shell library into a double-quoted bash assignment. The embedded
  # quotes and `if`/`fi` terminated the assignment early and left the enclosing
  # script unparseable:
  #
  #     line 1053: syntax error: unexpected end of file from `if' command on line 4
  #
  # so this check could never have run. `configModule.script` is the same path
  # the INI test below already sources.
  testFunctionsExist = pkgs.runCommand "test-config-functions-exist" {} ''
    echo "Testing config module functions exist..."

    lib_file="${configModule.script}/opt/firestream/scripts/libconfig.sh"

    if [ ! -s "$lib_file" ]; then
      echo "FAIL: config library file is missing or empty: $lib_file"
      exit 1
    fi

    for fn in ini_set ini_file_set ini_get ini_del ini_has_key python_conf_set python_conf_get url_encode url_decode airflow_encode_url generate_fernet_key process_fernet_key process_secret_key generate_secret_key ini_merge replace_placeholders load_env_file export_default; do
      if ! ${pkgs.gnugrep}/bin/grep -q "^[[:space:]]*$fn()" "$lib_file"; then
        echo "FAIL: function $fn not found in $lib_file"
        exit 1
      fi
    done

    echo "PASS: All expected functions are present"
    touch $out
  '';

  # Test INI file manipulation
  testIniOperations = pkgs.runCommand "test-config-ini-operations" {
    buildInputs = with pkgs; [ crudini coreutils ];
  } ''
    echo "Testing INI file operations..."

    # Source the config module
    source ${configModule.script}/opt/firestream/scripts/libconfig.sh

    # Create a test INI file
    cat > /tmp/test.ini << 'EOF'
[section1]
key1 = value1

[section2]
key2 = value2
EOF

    # Test ini_set
    ini_set "section1" "newkey" "newvalue" "/tmp/test.ini"
    if ! grep -q "newkey = newvalue" /tmp/test.ini; then
      echo "FAIL: ini_set did not add new key"
      exit 1
    fi
    echo "PASS: ini_set works"

    # Test ini_get
    result=$(ini_get "section1" "key1" "/tmp/test.ini")
    if [ "$result" != "value1" ]; then
      echo "FAIL: ini_get returned '$result' instead of 'value1'"
      exit 1
    fi
    echo "PASS: ini_get works"

    # Test ini_has_key
    if ! ini_has_key "section1" "key1" "/tmp/test.ini"; then
      echo "FAIL: ini_has_key failed for existing key"
      exit 1
    fi
    echo "PASS: ini_has_key works for existing key"

    if ini_has_key "section1" "nonexistent" "/tmp/test.ini"; then
      echo "FAIL: ini_has_key returned true for nonexistent key"
      exit 1
    fi
    echo "PASS: ini_has_key works for nonexistent key"

    # Test ini_del
    ini_del "section1" "newkey" "/tmp/test.ini"
    if grep -q "newkey" /tmp/test.ini; then
      echo "FAIL: ini_del did not remove key"
      exit 1
    fi
    echo "PASS: ini_del works"

    touch $out
  '';

  # Test URL encoding
  testUrlEncoding = pkgs.runCommand "test-config-url-encoding" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing URL encoding..."

    # Source the config module
    source ${configModule.script}/opt/firestream/scripts/libconfig.sh

    # Test url_encode
    result=$(url_encode "user@example.com")
    if [ "$result" != "user%40example.com" ]; then
      echo "FAIL: url_encode returned '$result' instead of 'user%40example.com'"
      exit 1
    fi
    echo "PASS: url_encode works"

    # Test with spaces
    result=$(url_encode "hello world")
    if [ "$result" != "hello%20world" ]; then
      echo "FAIL: url_encode space handling returned '$result'"
      exit 1
    fi
    echo "PASS: url_encode handles spaces"

    # Test airflow_encode_url (doubles %)
    result=$(airflow_encode_url "user@test")
    if [ "$result" != "user%%40test" ]; then
      echo "FAIL: airflow_encode_url returned '$result' instead of 'user%%40test'"
      exit 1
    fi
    echo "PASS: airflow_encode_url works"

    touch $out
  '';

  # Test secret key generation
  testSecretKeyGeneration = pkgs.runCommand "test-config-secret-keys" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing secret key generation..."

    # Source the config module
    source ${configModule.script}/opt/firestream/scripts/libconfig.sh

    # Test generate_secret_key default length
    result=$(generate_secret_key)
    if [ ''${#result} -ne 32 ]; then
      echo "FAIL: generate_secret_key default length is ''${#result}, expected 32"
      exit 1
    fi
    echo "PASS: generate_secret_key default length is 32"

    # Test generate_secret_key custom length
    result=$(generate_secret_key 16)
    if [ ''${#result} -ne 16 ]; then
      echo "FAIL: generate_secret_key custom length is ''${#result}, expected 16"
      exit 1
    fi
    echo "PASS: generate_secret_key custom length works"

    # Test generate_fernet_key
    result=$(generate_fernet_key "12345678901234567890123456789012")
    if [ -z "$result" ]; then
      echo "FAIL: generate_fernet_key returned empty"
      exit 1
    fi
    echo "PASS: generate_fernet_key works"

    touch $out
  '';

  # Test Python config manipulation
  testPythonConfig = pkgs.runCommand "test-config-python-conf" {
    buildInputs = with pkgs; [ coreutils gnugrep gnused ];
  } ''
    echo "Testing Python config manipulation..."

    # Source the config module
    source ${configModule.script}/opt/firestream/scripts/libconfig.sh

    # Create a test Python config file
    cat > /tmp/config.py << 'EOF'
# Test config
VAR1 = 'value1'
VAR2 = 100
# VAR3 = 'commented'
EOF

    # Test python_conf_set for new value
    # is_literal defaults to "no", i.e. the value IS quoted — which is what
    # the assertions below check. Passing "yes" here (as this test used to) means
    # "emit the value verbatim, unquoted", contradicting every assertion.
    python_conf_set "VAR1" "newvalue1" "/tmp/config.py"
    if ! grep -q "VAR1 = 'newvalue1'" /tmp/config.py; then
      cat /tmp/config.py
      echo "FAIL: python_conf_set did not update VAR1"
      exit 1
    fi
    echo "PASS: python_conf_set updates existing value"

    # Test python_conf_set for commented value
    python_conf_set "VAR3" "uncommented" "/tmp/config.py"
    if ! grep -q "VAR3 = 'uncommented'" /tmp/config.py; then
      cat /tmp/config.py
      echo "FAIL: python_conf_set did not uncomment VAR3"
      exit 1
    fi
    echo "PASS: python_conf_set uncomments values"

    # Test python_conf_set for new key
    python_conf_set "NEWVAR" "newval" "/tmp/config.py"
    if ! grep -q "NEWVAR = 'newval'" /tmp/config.py; then
      cat /tmp/config.py
      echo "FAIL: python_conf_set did not add NEWVAR"
      exit 1
    fi
    echo "PASS: python_conf_set adds new values"

    touch $out
  '';

in pkgs.runCommand "config-tests" {
  buildInputs = [ testFunctionsExist testIniOperations testUrlEncoding testSecretKeyGeneration testPythonConfig ];
} ''
  echo "================================================"
  echo "Config module tests completed successfully!"
  echo "================================================"
  touch $out
''
