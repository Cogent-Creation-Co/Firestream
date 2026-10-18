# validations.nix - Validation functions for input checking
{ pkgs, lib, logModule }:

let
  functions = ''
    # Import log functions
    ${logModule.functions}

    ########################
    # Check if the provided argument is an integer
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    # NOTE on ''${1-} rather than ''${1:?...}: this is a PREDICATE. `:?` fires on
    # an empty string as well as a missing one and, in a non-interactive shell,
    # it terminates the whole script instead of returning non-zero — so
    # `is_int ""` killed its caller rather than answering "no". Worse, the
    # diagnostic goes to stderr, so a caller that redirects stderr (the usual
    # shape for an expected-false check) saw a silent abort with no output.
    # A predicate must always be able to answer; only genuinely-required
    # arguments keep `:?`.
    is_int() {
        local -r int="''${1-}"
        if [[ "$int" =~ ^-?[0-9]+$ ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is a positive integer
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    # Predicate — see the note on `is_int` for why this is ''${1-} and not ''${1:?}.
    is_positive_int() {
        local -r int="''${1-}"
        if is_int "$int" && (( "''${int}" >= 0 )); then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is a boolean or is the string 'yes/true'
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    is_boolean_yes() {
        local -r bool="''${1:-}"
        # comparison is performed without regard to the case of alphabetic characters
        shopt -s nocasematch
        if [[ "$bool" = 1 || "$bool" =~ ^(yes|true)$ ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is a boolean yes/no value
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    is_yes_no_value() {
        local -r bool="''${1:-}"
        # comparison is performed without regard to the case of alphabetic characters
        shopt -s nocasematch
        if [[ "$bool" =~ ^(yes|no)$ ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is a boolean true/false value
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    is_true_false_value() {
        local -r bool="''${1:-}"
        # comparison is performed without regard to the case of alphabetic characters
        shopt -s nocasematch
        if [[ "$bool" =~ ^(true|false)$ ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is a boolean 1/0 value
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    is_1_0_value() {
        local -r bool="''${1:-}"
        if [[ "$bool" =~ ^[10]$ ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Check if the provided argument is an empty string or not defined
    # Arguments:
    #   $1 - Value to check
    # Returns:
    #   Boolean
    #########################
    is_empty_value() {
        local -r val="''${1:-}"
        if [[ -z "$val" ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Validate if the provided argument is a valid port
    # Arguments:
    #   $1 - Port to validate
    # Flags:
    #   -unprivileged - Reject ports < 1024
    # Returns:
    #   Boolean and error message
    #########################
    validate_port() {
        local value
        local unprivileged=0

        # Parse flags
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                -unprivileged)
                    unprivileged=1
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
                    break
                    ;;
            esac
            shift
        done

        if [[ "$#" -gt 1 ]]; then
            ${pkgs.coreutils}/bin/echo "too many arguments provided"
            return 2
        elif [[ "$#" -eq 0 ]]; then
            stderr_print "missing port argument"
            return 1
        else
            value=$1
        fi

        if [[ -z "$value" ]]; then
            ${pkgs.coreutils}/bin/echo "the value is empty"
            return 1
        else
            if ! is_int "$value"; then
                ${pkgs.coreutils}/bin/echo "value is not an integer"
                return 2
            elif [[ "$value" -lt 0 ]]; then
                ${pkgs.coreutils}/bin/echo "negative value provided"
                return 2
            elif [[ "$value" -eq 0 ]]; then
                # Port 0 is "pick any free port" to bind(2), never a valid
                # service port to validate or connect to. Kept as its own
                # branch so the message is accurate — it is not "negative".
                ${pkgs.coreutils}/bin/echo "port 0 is not a valid service port"
                return 2
            elif [[ "$value" -gt 65535 ]]; then
                ${pkgs.coreutils}/bin/echo "requested port is greater than 65535"
                return 2
            elif [[ "$unprivileged" = 1 && "$value" -lt 1024 ]]; then
                ${pkgs.coreutils}/bin/echo "privileged port requested"
                return 3
            fi
        fi
    }

    ########################
    # Validate if the provided argument is a valid IPv4 address
    # Arguments:
    #   $1 - IP to validate
    # Returns:
    #   Boolean
    #########################
    validate_ipv4() {
        local ip="''${1-}"
        local stat=1

        if [[ $ip =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
            read -r -a ip_array <<< "$(${pkgs.coreutils}/bin/tr '.' ' ' <<< "$ip")"
            [[ ''${ip_array[0]} -le 255 && ''${ip_array[1]} -le 255 \
                && ''${ip_array[2]} -le 255 && ''${ip_array[3]} -le 255 ]]
            stat=$?
        fi
        return $stat
    }

    ########################
    # Validate if the provided argument is a valid IPv6 address
    # Arguments:
    #   $1 - IP to validate
    # Returns:
    #   Boolean
    #########################
    validate_ipv6() {
        local ip="''${1-}"
        local stat=1
        local full_address_regex='^([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}$'
        local short_address_regex='^((([0-9a-fA-F]{1,4}:){0,6}[0-9a-fA-F]{1,4}){0,6}::(([0-9a-fA-F]{1,4}:){0,6}[0-9a-fA-F]{1,4}){0,6})$'

        if [[ $ip =~ $full_address_regex || $ip =~ $short_address_regex || $ip == "::" ]]; then
            stat=0
        fi
        return $stat
    }

    ########################
    # Validate if the provided argument is a valid IPv4 or IPv6 address
    # Arguments:
    #   $1 - IP to validate
    # Returns:
    #   Boolean
    #########################
    # Two defects fixed here, both of which made this function return success
    # for essentially any input:
    #   1. `$2` (the required IP version) was parsed by nobody — `validate_ip
    #      "192.168.1.1" 6` happily accepted an IPv4 address.
    #   2. `stat=$(validate_ipv6 "$ip")` captured validate_ipv6's *stdout*, not
    #      its exit status. validate_ipv6 prints nothing, so `stat` became the
    #      empty string and `return $stat` degenerated to `return`, i.e. the
    #      status of the last command — 0. The else-branch could never fail.
    #
    # Arguments:
    #   $1 - IP to validate
    #   $2 - required version: 4, 6, or empty/any for either
    validate_ip() {
        local ip="''${1-}"
        local version="''${2-}"

        case "$version" in
            4)
                validate_ipv4 "$ip"
                ;;
            6)
                validate_ipv6 "$ip"
                ;;
            ""|any)
                validate_ipv4 "$ip" || validate_ipv6 "$ip"
                ;;
            *)
                stderr_print "validate_ip: unknown IP version ''${version} (expected 4, 6 or empty)"
                return 1
                ;;
        esac
    }

    ########################
    # Validate a string format
    # Arguments:
    #   $1 - String to validate
    # Flags:
    #   -min-length - Minimum string length
    #   -max-length - Maximum string length
    # Returns:
    #   Boolean
    #########################
    validate_string() {
        local string
        local min_length=-1
        local max_length=-1

        # Parse flags
        while [ "$#" -gt 0 ]; do
            case "$1" in
                -min-length)
                    shift
                    min_length=''${1:-}
                    ;;
                -max-length)
                    shift
                    max_length=''${1:-}
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
                    string="$1"
                    ;;
            esac
            shift
        done

        if [[ "$min_length" -ge 0 ]] && [[ "''${#string}" -lt "$min_length" ]]; then
            ${pkgs.coreutils}/bin/echo "string length is less than $min_length"
            return 1
        fi
        if [[ "$max_length" -ge 0 ]] && [[ "''${#string}" -gt "$max_length" ]]; then
            ${pkgs.coreutils}/bin/echo "string length is great than $max_length"
            return 1
        fi
    }
  '';
in
{
  meta = {
    name = "libvalidations";
    description = "Validation functions for input checking";
    version = "1.0.0";
  };

  imports = [ logModule ];
  runtimeDeps = with pkgs; [ coreutils ];
  inherit functions;
  exports = [
    "is_int"
    "is_positive_int"
    "is_boolean_yes"
    "is_yes_no_value"
    "is_true_false_value"
    "is_1_0_value"
    "is_empty_value"
    "validate_port"
    "validate_ipv4"
    "validate_ipv6"
    "validate_ip"
    "validate_string"
  ];

  script = pkgs.writeTextDir "opt/firestream/scripts/libvalidations.sh" ''
    #!/bin/bash
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.
    ${functions}
  '';
}
