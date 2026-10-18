# net.nix - Network utilities and connectivity functions
{ pkgs, lib, logModule, validationsModule, waitForPortPkg }:

let
  functions = ''
    # Import dependencies
    ${logModule.functions}
    ${validationsModule.functions}

    ########################
    # Perform DNS lookup
    # Arguments:
    #   $1 - hostname
    # Returns:
    #   First IP address or empty string
    #########################
    dns_lookup() {
        local host="''${1:?hostname is missing}"
        # `getent` is its own nixpkgs package — glibc.bin ships getconf but NOT
        # getent, so ${"$"}{pkgs.glibc.bin}/bin/getent was a path that never existed
        # and every DNS lookup failed with "No such file or directory".
        ${pkgs.getent}/bin/getent ahosts "$host" | ${pkgs.gawk}/bin/awk 'NR==1 {print $1}'
    }

    ########################
    # Wait for DNS lookup to succeed
    # Arguments:
    #   $1 - hostname
    # Flags:
    #   --tries - Number of retry attempts (default: 12)
    #   --sleep - Seconds to sleep between retries (default: 5)
    # Returns:
    #   Boolean
    #########################
    wait_for_dns_lookup() {
        local host=""
        local tries=12
        local sleep_time=5

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --tries)
                    shift
                    tries="''${1:?missing tries value}"
                    ;;
                --sleep)
                    shift
                    sleep_time="''${1:?missing sleep value}"
                    ;;
                --)
                    shift
                    break
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    host="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$host" ]]; then
            stderr_print "hostname is required"
            return 1
        fi

        for ((i = 1; i <= tries; i++)); do
            debug "DNS lookup attempt $i/$tries for $host"
            local result
            result=$(dns_lookup "$host")
            if [[ -n "$result" ]]; then
                debug "DNS lookup succeeded: $host -> $result"
                return 0
            fi
            ${pkgs.coreutils}/bin/sleep "$sleep_time"
        done

        error "DNS lookup failed for $host after $tries attempts"
        return 1
    }

    ########################
    # Get machine's IP address
    # Arguments:
    #   None
    # Returns:
    #   Machine's IP address
    #########################
    get_machine_ip() {
        local hostname
        # NOT `coreutils/bin/hostname` — coreutils does not ship `hostname`
        # (it has `hostid`), so the old path was a guaranteed
        # "No such file or directory" for every caller. `uname -n` is the
        # coreutils-provided way to get the nodename.
        hostname=$(${pkgs.coreutils}/bin/uname -n)
        local ip
        ip=$(dns_lookup "$hostname")
        if [[ -z "$ip" ]]; then
            # An unresolvable own-hostname is normal in a build sandbox and on
            # hosts with no DNS entry for themselves. Fall back to loopback
            # rather than returning empty, so callers always get a usable
            # address instead of silently interpolating "".
            warn "get_machine_ip: could not resolve ''${hostname}, falling back to 127.0.0.1"
            ip="127.0.0.1"
        fi
        echo "$ip"
    }

    ########################
    # Check if hostname resolves
    # Arguments:
    #   $1 - hostname
    # Returns:
    #   Boolean
    #########################
    is_hostname_resolved() {
        local host="''${1:?hostname is missing}"
        local result
        result=$(dns_lookup "$host")
        [[ -n "$result" ]]
    }

    ########################
    # Parse URI into components
    #
    # Two equivalent call shapes are accepted:
    #   parse_uri "$uri" --host      (flag form, Firestream-native)
    #   parse_uri "$uri" host        (positional form, matches upstream Bitnami
    #                                 libnet.sh so vendored scripts port over
    #                                 unchanged)
    #
    # Arguments:
    #   $1 - URI string
    #   $2 - component name, when using the positional form
    # Flags:
    #   --scheme - Extract scheme
    #   --authority - Extract authority
    #   --userinfo - Extract userinfo
    #   --host - Extract host
    #   --port - Extract port
    #   --path - Extract path
    #   --query - Extract query
    #   --fragment - Extract fragment
    # Returns:
    #   Requested URI component
    #########################
    parse_uri() {
        local uri=""
        local component="all"

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --scheme)
                    component="scheme"
                    ;;
                --authority)
                    component="authority"
                    ;;
                --userinfo)
                    component="userinfo"
                    ;;
                --host)
                    component="host"
                    ;;
                --port)
                    component="port"
                    ;;
                --path)
                    component="path"
                    ;;
                --query)
                    component="query"
                    ;;
                --fragment)
                    component="fragment"
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    if [[ -z "$uri" ]]; then
                        uri="$1"
                    else
                        # Positional component (Bitnami form): the second bare
                        # argument names the component to extract.
                        case "$1" in
                            scheme|authority|userinfo|host|port|path|query|fragment|all)
                                component="$1"
                                ;;
                            *)
                                stderr_print "unknown URI component $1"
                                return 1
                                ;;
                        esac
                    fi
                    ;;
            esac
            shift
        done

        if [[ -z "$uri" ]]; then
            stderr_print "URI is required"
            return 1
        fi

        # URI regex pattern: scheme://[userinfo@]host[:port][/path][?query][#fragment]
        local scheme=""
        local authority=""
        local userinfo=""
        local host=""
        local port=""
        local path=""
        local query=""
        local fragment=""

        # Extract fragment first
        if [[ "$uri" =~ ^([^#]*)#(.*)$ ]]; then
            uri="''${BASH_REMATCH[1]}"
            fragment="''${BASH_REMATCH[2]}"
        fi

        # Extract query
        if [[ "$uri" =~ ^([^?]*)\?(.*)$ ]]; then
            uri="''${BASH_REMATCH[1]}"
            query="''${BASH_REMATCH[2]}"
        fi

        # Extract scheme
        if [[ "$uri" =~ ^([a-zA-Z][a-zA-Z0-9+.-]*):// ]]; then
            scheme="''${BASH_REMATCH[1]}"
            uri="''${uri#*://}"
        fi

        # Extract path
        if [[ "$uri" =~ ^([^/]*)(/.*)$ ]]; then
            authority="''${BASH_REMATCH[1]}"
            path="''${BASH_REMATCH[2]}"
        else
            authority="$uri"
        fi

        # Extract userinfo
        if [[ "$authority" =~ ^(.*)@(.*)$ ]]; then
            userinfo="''${BASH_REMATCH[1]}"
            authority="''${BASH_REMATCH[2]}"
        fi

        # Extract port
        if [[ "$authority" =~ ^(.*):([0-9]+)$ ]]; then
            host="''${BASH_REMATCH[1]}"
            port="''${BASH_REMATCH[2]}"
        else
            host="$authority"
        fi

        case "$component" in
            scheme)
                ${pkgs.coreutils}/bin/echo "$scheme"
                ;;
            authority)
                ${pkgs.coreutils}/bin/echo "$authority"
                ;;
            userinfo)
                ${pkgs.coreutils}/bin/echo "$userinfo"
                ;;
            host)
                ${pkgs.coreutils}/bin/echo "$host"
                ;;
            port)
                ${pkgs.coreutils}/bin/echo "$port"
                ;;
            path)
                ${pkgs.coreutils}/bin/echo "$path"
                ;;
            query)
                ${pkgs.coreutils}/bin/echo "$query"
                ;;
            fragment)
                ${pkgs.coreutils}/bin/echo "$fragment"
                ;;
            all)
                ${pkgs.coreutils}/bin/echo "scheme=$scheme"
                ${pkgs.coreutils}/bin/echo "authority=$authority"
                ${pkgs.coreutils}/bin/echo "userinfo=$userinfo"
                ${pkgs.coreutils}/bin/echo "host=$host"
                ${pkgs.coreutils}/bin/echo "port=$port"
                ${pkgs.coreutils}/bin/echo "path=$path"
                ${pkgs.coreutils}/bin/echo "query=$query"
                ${pkgs.coreutils}/bin/echo "fragment=$fragment"
                ;;
        esac
    }

    ########################
    # Wait for HTTP endpoint to respond
    # Arguments:
    #   $1 - URL
    # Flags:
    #   --tries - Number of retry attempts (default: 12)
    #   --sleep - Seconds to sleep between retries (default: 5)
    #   --status - Expected HTTP status code (default: 200)
    # Returns:
    #   Boolean
    #########################
    wait_for_http_connection() {
        local url=""
        local tries=12
        local sleep_time=5
        local expected_status=200

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --tries)
                    shift
                    tries="''${1:?missing tries value}"
                    ;;
                --sleep)
                    shift
                    sleep_time="''${1:?missing sleep value}"
                    ;;
                --status)
                    shift
                    expected_status="''${1:?missing status value}"
                    ;;
                --)
                    shift
                    break
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    url="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$url" ]]; then
            stderr_print "URL is required"
            return 1
        fi

        for ((i = 1; i <= tries; i++)); do
            debug "HTTP connection attempt $i/$tries for $url"
            local status
            status=$(${pkgs.curl}/bin/curl -s -o /dev/null -w "%{http_code}" "$url" 2>/dev/null || ${pkgs.coreutils}/bin/echo "000")
            if [[ "$status" -eq "$expected_status" ]]; then
                debug "HTTP connection succeeded: $url (status: $status)"
                return 0
            fi
            ${pkgs.coreutils}/bin/sleep "$sleep_time"
        done

        error "HTTP connection failed for $url after $tries attempts"
        return 1
    }

    ########################
    # Retry a command until it succeeds or max attempts reached
    # Arguments:
    #   $1 - command to execute (as string)
    # Flags:
    #   --tries - Number of retry attempts (default: 30)
    #   --sleep - Seconds to sleep between retries (default: 5)
    # Returns:
    #   Boolean
    #########################
    retry_while() {
        local cmd=""
        local tries=30
        local sleep_time=5

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --tries)
                    shift
                    tries="''${1:?missing tries value}"
                    ;;
                --sleep)
                    shift
                    sleep_time="''${1:?missing sleep value}"
                    ;;
                --)
                    shift
                    break
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    cmd="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$cmd" ]]; then
            stderr_print "command is required"
            return 1
        fi

        local attempt=0
        while ! eval "$cmd"; do
            ((++attempt))  # Pre-increment to avoid set -e exit when attempt=0
            if [[ $attempt -ge $tries ]]; then
                error "Command failed after $tries attempts: $cmd"
                return 1
            fi
            debug "Retry attempt $attempt/$tries for: $cmd"
            ${pkgs.coreutils}/bin/sleep "$sleep_time"
        done
        return 0
    }

    ########################
    # Retry a command with timeout
    # Arguments:
    #   $1 - command to execute (as string)
    # Flags:
    #   --timeout - Total timeout in seconds (default: 120)
    #   --sleep - Seconds to sleep between retries (default: 5)
    # Returns:
    #   Boolean
    #########################
    retry_with_timeout() {
        local cmd=""
        local timeout=120
        local sleep_time=5

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --timeout)
                    shift
                    timeout="''${1:?missing timeout value}"
                    ;;
                --sleep)
                    shift
                    sleep_time="''${1:?missing sleep value}"
                    ;;
                --)
                    shift
                    break
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    cmd="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$cmd" ]]; then
            stderr_print "command is required"
            return 1
        fi

        local start_time
        start_time=$(${pkgs.coreutils}/bin/date +%s)
        local elapsed=0

        while ! eval "$cmd"; do
            elapsed=$(($(${pkgs.coreutils}/bin/date +%s) - start_time))
            if [[ $elapsed -ge $timeout ]]; then
                error "Command timed out after ''${timeout}s: $cmd"
                return 1
            fi
            debug "Retry (elapsed: ''${elapsed}s/''${timeout}s) for: $cmd"
            ${pkgs.coreutils}/bin/sleep "$sleep_time"
        done
        return 0
    }

    ########################
    # Wait for TCP port to reach a desired state
    # This is a thin wrapper around the Rust wait-for-port binary.
    #
    # Usage:
    #   wait-for-port PORT [--host HOST] [--state inuse|free] [--timeout SECS] [--verbose]
    #
    # Arguments:
    #   PORT - Port number to check (1-65535)
    #
    # Flags:
    #   --host HOST      - Target host (default: localhost)
    #   --state STATE    - Desired port state: inuse or free (default: inuse)
    #   --timeout SECS   - Timeout in seconds (default: 30)
    #   --verbose        - Enable verbose output
    #
    # Returns:
    #   0 - Port reached desired state
    #   1 - Timeout or error
    #
    # Examples:
    #   wait-for-port 5432 --host db.example.com --timeout 120
    #   wait-for-port 8080 --state free --timeout 5
    #########################
    wait_for_port() {
        # Thin wrapper - pass all arguments to Rust binary
        ${waitForPortPkg}/bin/wait-for-port "$@"
    }
  '';
in
{
  meta = {
    name = "libnet";
    description = "Network utilities and connectivity functions";
    version = "1.0.0";
  };

  imports = [ logModule validationsModule ];
  runtimeDeps = (with pkgs; [
    coreutils
    glibc.bin
    gawk
    findutils
    nettools
    curl
    netcat-gnu  # Keep for other utilities that may need it
  ]) ++ [
    waitForPortPkg  # Rust-based port checker
  ];
  inherit functions;
  exports = [
    "dns_lookup"
    "wait_for_dns_lookup"
    "get_machine_ip"
    "is_hostname_resolved"
    "parse_uri"
    "wait_for_http_connection"
    "retry_while"
    "retry_with_timeout"
    "wait_for_port"
  ];

  script = pkgs.writeTextDir "opt/firestream/scripts/libnet.sh" ''
    #!/bin/bash
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.
    ${functions}
  '';
}
