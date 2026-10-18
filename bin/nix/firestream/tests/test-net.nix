{ pkgs, firestream }:

# Tests for the net module (lib/net.nix)
# Copyright Firestream. MIT License.
#
# NOTE on scope: this suite previously asserted against `resolve_hostname_ip`,
# `wait_for_host` and `get_port_from_url`, none of which exist in lib/net.nix
# (or anywhere else in the tree) and none of which had a single caller. They
# were removed rather than implemented — see IMPLEMENTATION_STATUS.md. Where the
# library already provides the same capability under a different name the
# assertion was re-pointed rather than dropped:
#   resolve_hostname_ip -> dns_lookup
#   get_port_from_url   -> parse_uri "$uri" port
#
# `parse_uri` is exercised in BOTH its positional (upstream-Bitnami-compatible)
# and flag forms, since both are supported.

pkgs.runCommand "test-net" {} ''
  export HOME=$TMPDIR

  cat > $TMPDIR/test.sh << 'SCRIPT'
  ${firestream.lib.net.functions}

  # Test parse_uri — component extraction
  [[ "$(parse_uri "http://example.com:8080/path" scheme)" == "http" ]] \
    || { echo "FAIL: parse_uri scheme"; exit 1; }
  [[ "$(parse_uri "http://example.com:8080/path" host)" == "example.com" ]] \
    || { echo "FAIL: parse_uri host"; exit 1; }
  [[ "$(parse_uri "http://example.com:8080/path" port)" == "8080" ]] \
    || { echo "FAIL: parse_uri port"; exit 1; }
  [[ "$(parse_uri "http://example.com:8080/path" path)" == "/path" ]] \
    || { echo "FAIL: parse_uri path"; exit 1; }

  # The flag form must stay equivalent to the positional form.
  [[ "$(parse_uri "http://example.com:8080/path" --host)" == "example.com" ]] \
    || { echo "FAIL: parse_uri --host flag form"; exit 1; }

  # Test parse_uri with query string
  [[ "$(parse_uri "http://example.com/path?key=value" path)" == "/path" ]] \
    || { echo "FAIL: parse_uri path with query"; exit 1; }
  [[ "$(parse_uri "http://example.com/path?key=value" query)" == "key=value" ]] \
    || { echo "FAIL: parse_uri query"; exit 1; }

  # An unknown component is an error, not a silent empty string.
  ! parse_uri "http://example.com" bogus 2>/dev/null \
    || { echo "FAIL: parse_uri should reject an unknown component"; exit 1; }

  # Test dns_lookup — localhost always resolves, even in the build sandbox.
  ip=$(dns_lookup localhost)
  [[ -n "$ip" ]] || { echo "FAIL: dns_lookup localhost should return an address"; exit 1; }

  # get_machine_ip must always yield an address (it falls back to loopback when
  # the node's own hostname does not resolve, which is the sandbox's case).
  machine_ip=$(get_machine_ip 2>/dev/null)
  [[ -n "$machine_ip" ]] || { echo "FAIL: get_machine_ip should return a value"; exit 1; }

  # Test validate_ip (IPv4)
  validate_ip "192.168.1.1" 4 || { echo "FAIL: validate_ip IPv4"; exit 1; }
  ! validate_ip "invalid" 4 2>/dev/null || { echo "FAIL: validate_ip should reject invalid IPv4"; exit 1; }

  # Test validate_ip (IPv6)
  validate_ip "::1" 6 || { echo "FAIL: validate_ip IPv6"; exit 1; }
  ! validate_ip "192.168.1.1" 6 2>/dev/null || { echo "FAIL: validate_ip should reject IPv4 when expecting IPv6"; exit 1; }

  # Test validate_ip (any version)
  validate_ip "192.168.1.1" || { echo "FAIL: validate_ip any (IPv4)"; exit 1; }
  validate_ip "::1" || { echo "FAIL: validate_ip any (IPv6)"; exit 1; }
  ! validate_ip "not-an-ip" 2>/dev/null || { echo "FAIL: validate_ip should reject invalid"; exit 1; }

  # is_hostname_resolved is the boolean sibling of dns_lookup.
  is_hostname_resolved localhost || { echo "FAIL: is_hostname_resolved localhost"; exit 1; }

  echo "All net tests passed!"
  SCRIPT

  ${pkgs.bash}/bin/bash $TMPDIR/test.sh
  touch $out
''
