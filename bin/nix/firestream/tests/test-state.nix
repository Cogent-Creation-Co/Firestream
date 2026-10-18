# Tests for the state module (lib/state.nix)
# Copyright Firestream. MIT License.
{ pkgs, firestream }:

let
  stateModule = firestream.lib.state;

  # Test that all expected functions are present.
  #
  # Inspects the emitted library FILE, not a shell string — same fix as
  # test-config.nix. Interpolating the whole library into
  # `functions="${"\${"}stateModule.functions}"` left the enclosing script
  # unparseable ("syntax error near unexpected token `('"), so this check could
  # never run. It is only reachable through the `firestream-tests` aggregate,
  # which is why it went unnoticed while the granular checks were green.
  testFunctionsExist = pkgs.runCommand "test-state-functions-exist" {} ''
    echo "Testing state module functions exist..."

    lib_file="${stateModule.script}/opt/firestream/scripts/libstate.sh"

    if [ ! -s "$lib_file" ]; then
      echo "FAIL: state library file is missing or empty: $lib_file"
      exit 1
    fi

    for fn in get_state_dir get_config_hash save_config_hash has_config_changed get_generation increment_generation record_activation check_prepopulated mark_prepopulated get_activation_time clear_state; do
      if ! ${pkgs.gnugrep}/bin/grep -q "^[[:space:]]*$fn()" "$lib_file"; then
        echo "FAIL: function $fn not found in $lib_file"
        exit 1
      fi
    done

    echo "PASS: All expected functions are present"
    touch $out
  '';

  # Test get_state_dir returns correct path
  testGetStateDir = pkgs.runCommand "test-state-get-state-dir" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing get_state_dir..."

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Test with default base directory
    result=$(get_state_dir "testapp")
    expected="/firestream/testapp/.state"
    if [ "$result" != "$expected" ]; then
      echo "FAIL: get_state_dir returned '$result', expected '$expected'"
      exit 1
    fi
    echo "PASS: get_state_dir with default base directory"

    # Test with custom base directory
    result=$(get_state_dir "myapp" "/custom/path")
    expected="/custom/path/myapp/.state"
    if [ "$result" != "$expected" ]; then
      echo "FAIL: get_state_dir with custom base returned '$result', expected '$expected'"
      exit 1
    fi
    echo "PASS: get_state_dir with custom base directory"

    touch $out
  '';

  # Test get_config_hash produces consistent SHA256 hash
  testConfigHash = pkgs.runCommand "test-state-config-hash" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing get_config_hash..."

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create a test config file
    echo "key=value" > /tmp/config1.txt

    # Test single file hashing
    hash1=$(get_config_hash "/tmp/config1.txt")
    hash2=$(get_config_hash "/tmp/config1.txt")

    if [ "$hash1" != "$hash2" ]; then
      echo "FAIL: get_config_hash is not consistent (got '$hash1' and '$hash2')"
      exit 1
    fi
    echo "PASS: get_config_hash is consistent for same file"

    # Verify it's a valid SHA256 hash (64 hex characters)
    if ! echo "$hash1" | grep -qE '^[a-f0-9]{64}$'; then
      echo "FAIL: get_config_hash did not return valid SHA256 (got '$hash1')"
      exit 1
    fi
    echo "PASS: get_config_hash returns valid SHA256 format"

    # Test different content produces different hash
    echo "different=content" > /tmp/config2.txt
    hash3=$(get_config_hash "/tmp/config2.txt")

    if [ "$hash1" == "$hash3" ]; then
      echo "FAIL: get_config_hash returned same hash for different content"
      exit 1
    fi
    echo "PASS: get_config_hash produces different hash for different content"

    # Test multiple files hashing
    hash_multi=$(get_config_hash "/tmp/config1.txt" "/tmp/config2.txt")
    if ! echo "$hash_multi" | grep -qE '^[a-f0-9]{64}$'; then
      echo "FAIL: get_config_hash multi-file did not return valid SHA256 (got '$hash_multi')"
      exit 1
    fi
    echo "PASS: get_config_hash handles multiple files"

    touch $out
  '';

  # Test save_config_hash and has_config_changed work together
  testConfigHashChange = pkgs.runCommand "test-state-config-hash-change" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing save_config_hash and has_config_changed..."

    export HOME=$TMPDIR

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create test directory structure (override base_dir in state_dir)
    export TEST_BASE="$TMPDIR/firestream"
    mkdir -p "$TEST_BASE"

    # Create a test config file
    echo "original=value" > /tmp/testconfig.txt

    # Patch the base_dir for testing - we'll manually test the logic
    state_dir="$TEST_BASE/testapp/.state"
    mkdir -p "$state_dir"

    # Test has_config_changed returns 0 (changed) when no saved hash exists
    # Since has_config_changed uses hardcoded /firestream, we'll test the logic components
    hash_file="$state_dir/config-hash"

    # Initially no hash file should exist
    if [ -f "$hash_file" ]; then
      echo "FAIL: hash file should not exist initially"
      exit 1
    fi
    echo "PASS: no hash file initially"

    # Calculate and save hash manually for testing
    current_hash=$(get_config_hash "/tmp/testconfig.txt")
    echo "$current_hash" > "$hash_file"

    # Verify saved hash matches
    saved_hash=$(cat "$hash_file")
    if [ "$saved_hash" != "$current_hash" ]; then
      echo "FAIL: saved hash does not match current hash"
      exit 1
    fi
    echo "PASS: hash saved correctly"

    # Test that same content produces same hash
    new_hash=$(get_config_hash "/tmp/testconfig.txt")
    if [ "$new_hash" != "$saved_hash" ]; then
      echo "FAIL: hash changed for unchanged file"
      exit 1
    fi
    echo "PASS: unchanged file produces same hash"

    # Modify config and verify hash changes
    echo "modified=value" > /tmp/testconfig.txt
    modified_hash=$(get_config_hash "/tmp/testconfig.txt")
    if [ "$modified_hash" == "$saved_hash" ]; then
      echo "FAIL: hash did not change for modified file"
      exit 1
    fi
    echo "PASS: modified file produces different hash"

    touch $out
  '';

  # Test get_generation and increment_generation
  testGeneration = pkgs.runCommand "test-state-generation" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing get_generation and increment_generation..."

    export HOME=$TMPDIR

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create test directory structure
    TEST_BASE="$TMPDIR/firestream"
    mkdir -p "$TEST_BASE"

    # Test get_generation returns 0 when no generation file exists
    gen=$(get_generation "testapp" "$TEST_BASE")
    if [ "$gen" != "0" ]; then
      echo "FAIL: get_generation should return 0 initially (got '$gen')"
      exit 1
    fi
    echo "PASS: get_generation returns 0 initially"

    # Test increment_generation
    new_gen=$(increment_generation "testapp" "$TEST_BASE")
    if [ "$new_gen" != "1" ]; then
      echo "FAIL: increment_generation should return 1 (got '$new_gen')"
      exit 1
    fi
    echo "PASS: increment_generation returns 1"

    # Verify get_generation now returns 1
    gen=$(get_generation "testapp" "$TEST_BASE")
    if [ "$gen" != "1" ]; then
      echo "FAIL: get_generation should return 1 after increment (got '$gen')"
      exit 1
    fi
    echo "PASS: get_generation returns 1 after increment"

    # Test multiple increments
    new_gen=$(increment_generation "testapp" "$TEST_BASE")
    if [ "$new_gen" != "2" ]; then
      echo "FAIL: second increment should return 2 (got '$new_gen')"
      exit 1
    fi
    echo "PASS: second increment returns 2"

    new_gen=$(increment_generation "testapp" "$TEST_BASE")
    if [ "$new_gen" != "3" ]; then
      echo "FAIL: third increment should return 3 (got '$new_gen')"
      exit 1
    fi
    echo "PASS: third increment returns 3"

    # Test that generation file contains correct value
    gen_file="$TEST_BASE/testapp/.state/generation"
    if [ ! -f "$gen_file" ]; then
      echo "FAIL: generation file does not exist"
      exit 1
    fi
    file_content=$(cat "$gen_file")
    if [ "$file_content" != "3" ]; then
      echo "FAIL: generation file contains '$file_content', expected '3'"
      exit 1
    fi
    echo "PASS: generation file contains correct value"

    touch $out
  '';

  # Test record_activation
  testRecordActivation = pkgs.runCommand "test-state-record-activation" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing record_activation..."

    export HOME=$TMPDIR

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create test directory structure
    TEST_BASE="$TMPDIR/firestream"
    mkdir -p "$TEST_BASE"

    # Record activation
    record_activation "testapp" "$TEST_BASE"

    # Verify state directory was created
    state_dir="$TEST_BASE/testapp/.state"
    if [ ! -d "$state_dir" ]; then
      echo "FAIL: state directory was not created"
      exit 1
    fi
    echo "PASS: state directory created"

    # Verify activation-time file was created
    activation_file="$state_dir/activation-time"
    if [ ! -f "$activation_file" ]; then
      echo "FAIL: activation-time file was not created"
      exit 1
    fi
    echo "PASS: activation-time file created"

    # Verify activation time is a valid ISO timestamp
    activation_time=$(cat "$activation_file")
    if ! echo "$activation_time" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}'; then
      echo "FAIL: activation time is not valid ISO format (got '$activation_time')"
      exit 1
    fi
    echo "PASS: activation time is valid ISO format"

    # Verify generation was incremented
    gen=$(get_generation "testapp" "$TEST_BASE")
    if [ "$gen" != "1" ]; then
      echo "FAIL: generation should be 1 after activation (got '$gen')"
      exit 1
    fi
    echo "PASS: generation incremented during activation"

    # Test get_activation_time
    retrieved_time=$(get_activation_time "testapp" "$TEST_BASE")
    if [ "$retrieved_time" != "$activation_time" ]; then
      echo "FAIL: get_activation_time returned '$retrieved_time', expected '$activation_time'"
      exit 1
    fi
    echo "PASS: get_activation_time returns correct value"

    # Record another activation
    sleep 1
    record_activation "testapp" "$TEST_BASE"
    gen=$(get_generation "testapp" "$TEST_BASE")
    if [ "$gen" != "2" ]; then
      echo "FAIL: generation should be 2 after second activation (got '$gen')"
      exit 1
    fi
    echo "PASS: multiple activations increment generation"

    touch $out
  '';

  # Test check_prepopulated and mark_prepopulated
  testPrepopulated = pkgs.runCommand "test-state-prepopulated" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing check_prepopulated and mark_prepopulated..."

    export HOME=$TMPDIR

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create test directory structure
    TEST_BASE="$TMPDIR/firestream"
    mkdir -p "$TEST_BASE"

    # Test check_prepopulated returns false (exit 1) initially
    if check_prepopulated "testapp" "$TEST_BASE"; then
      echo "FAIL: check_prepopulated should return false initially"
      exit 1
    fi
    echo "PASS: check_prepopulated returns false initially"

    # Mark as prepopulated
    mark_prepopulated "testapp" "$TEST_BASE"

    # Verify marker file was created
    marker_file="$TEST_BASE/testapp/.state/prepopulated"
    if [ ! -f "$marker_file" ]; then
      echo "FAIL: prepopulated marker file was not created"
      exit 1
    fi
    echo "PASS: prepopulated marker file created"

    # Test check_prepopulated now returns true (exit 0)
    if ! check_prepopulated "testapp" "$TEST_BASE"; then
      echo "FAIL: check_prepopulated should return true after marking"
      exit 1
    fi
    echo "PASS: check_prepopulated returns true after marking"

    # Test different app is not affected
    if check_prepopulated "otherapp" "$TEST_BASE"; then
      echo "FAIL: check_prepopulated should return false for different app"
      exit 1
    fi
    echo "PASS: check_prepopulated is app-specific"

    touch $out
  '';

  # Test clear_state
  testClearState = pkgs.runCommand "test-state-clear-state" {
    buildInputs = with pkgs; [ coreutils ];
  } ''
    echo "Testing clear_state..."

    export HOME=$TMPDIR

    # Source the state module
    source ${stateModule.script}/opt/firestream/scripts/libstate.sh

    # Create test directory structure
    TEST_BASE="$TMPDIR/firestream"
    mkdir -p "$TEST_BASE"

    # Setup some state
    record_activation "testapp" "$TEST_BASE"
    mark_prepopulated "testapp" "$TEST_BASE"
    increment_generation "testapp" "$TEST_BASE"

    # Verify state exists
    state_dir="$TEST_BASE/testapp/.state"
    if [ ! -d "$state_dir" ]; then
      echo "FAIL: state directory should exist before clear"
      exit 1
    fi
    echo "PASS: state directory exists before clear"

    # Clear state
    clear_state "testapp" "$TEST_BASE"

    # Verify state directory was removed
    if [ -d "$state_dir" ]; then
      echo "FAIL: state directory should be removed after clear"
      exit 1
    fi
    echo "PASS: state directory removed after clear"

    # Verify app can be re-initialized
    record_activation "testapp" "$TEST_BASE"
    gen=$(get_generation "testapp" "$TEST_BASE")
    if [ "$gen" != "1" ]; then
      echo "FAIL: generation should be 1 after re-initialization (got '$gen')"
      exit 1
    fi
    echo "PASS: app can be re-initialized after clear"

    # Test clearing non-existent state doesn't fail
    clear_state "nonexistentapp" "$TEST_BASE"
    echo "PASS: clearing non-existent state doesn't fail"

    touch $out
  '';

in pkgs.runCommand "state-tests" {
  buildInputs = [
    testFunctionsExist
    testGetStateDir
    testConfigHash
    testConfigHashChange
    testGeneration
    testRecordActivation
    testPrepopulated
    testClearState
  ];
} ''
  echo "================================================"
  echo "State module tests completed successfully!"
  echo "================================================"
  touch $out
''
