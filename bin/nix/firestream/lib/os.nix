# os.nix - Operating system utilities and user management
{ pkgs, lib, logModule, fsModule, validationsModule }:

let
  functions = ''
    # Import dependencies
    ${logModule.functions}
    ${fsModule.functions}
    ${validationsModule.functions}

    ########################
    # Check if user exists
    # Arguments:
    #   $1 - username
    # Returns:
    #   Boolean
    #########################
    user_exists() {
        local user="''${1:?user is missing}"
        ${pkgs.coreutils}/bin/id "$user" >/dev/null 2>&1
    }

    ########################
    # Check if group exists
    # Arguments:
    #   $1 - group name
    # Returns:
    #   Boolean
    #########################
    group_exists() {
        local group="''${1:?group is missing}"
        # See the note in lib/net.nix: getent is a separate package, not part of
        # glibc.bin. The old path never existed, so group_exists always said no.
        ${pkgs.getent}/bin/getent group "$group" >/dev/null 2>&1
    }

    ########################
    # Ensure user exists, creating if necessary
    # Arguments:
    #   $1 - username
    # Flags:
    #   --group - Primary group name
    #   --uid - User ID
    #   --home - Home directory
    #   --system - Create as system user
    # Returns:
    #   None
    #########################
    ensure_user_exists() {
        local username=""
        local group=""
        local uid=""
        local home=""
        local system=""

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --group)
                    shift
                    group="''${1:?missing group value}"
                    ;;
                --uid)
                    shift
                    uid="''${1:?missing uid value}"
                    ;;
                --home)
                    shift
                    home="''${1:?missing home value}"
                    ;;
                --system)
                    system="yes"
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    username="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$username" ]]; then
            stderr_print "username is required"
            return 1
        fi

        if ! user_exists "$username"; then
            debug "Creating user $username"
            local -a cmd=(
                "${pkgs.shadow}/bin/useradd"
            )
            [[ -n "$group" ]] && cmd+=("-g" "$group")
            [[ -n "$uid" ]] && cmd+=("-u" "$uid")
            [[ -n "$home" ]] && cmd+=("-d" "$home")
            [[ -n "$system" ]] && cmd+=("--system")
            cmd+=("$username")
            "''${cmd[@]}"
        fi
    }

    ########################
    # Ensure group exists, creating if necessary
    # Arguments:
    #   $1 - group name
    # Flags:
    #   --gid - Group ID
    #   --system - Create as system group
    # Returns:
    #   None
    #########################
    ensure_group_exists() {
        local groupname=""
        local gid=""
        local system=""

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --gid)
                    shift
                    gid="''${1:?missing gid value}"
                    ;;
                --system)
                    system="yes"
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    groupname="$1"
                    ;;
            esac
            shift
        done

        if [[ -z "$groupname" ]]; then
            stderr_print "group name is required"
            return 1
        fi

        if ! group_exists "$groupname"; then
            debug "Creating group $groupname"
            local -a cmd=(
                "${pkgs.shadow}/bin/groupadd"
            )
            [[ -n "$gid" ]] && cmd+=("-g" "$gid")
            [[ -n "$system" ]] && cmd+=("--system")
            cmd+=("$groupname")
            "''${cmd[@]}"
        fi
    }

    ########################
    # Check if running as root
    # Arguments:
    #   None
    # Returns:
    #   Boolean
    #########################
    am_i_root() {
        if [[ "$(${pkgs.coreutils}/bin/id -u)" -eq 0 ]]; then
            true
        else
            false
        fi
    }

    ########################
    # Get total memory in MB
    # Arguments:
    #   None
    # Returns:
    #   Memory in MB
    #########################
    get_total_memory() {
        ${pkgs.gawk}/bin/awk '/MemTotal/ {printf "%d", $2/1024}' /proc/meminfo
    }

    ########################
    # Get the number of CPUs available
    # Arguments:
    #   None
    # Returns:
    #   Positive integer
    #########################
    get_total_cpus() {
        ${pkgs.coreutils}/bin/nproc
    }

    ########################
    # Read a field out of /etc/os-release
    #
    # Ported from the vendored Bitnami reference (see
    # src/containers/firestream/spark/4.0/debian-12/prebuildfs/opt/bitnami/scripts/libos.sh)
    # so the flag vocabulary matches what Bitnami scripts already expect, plus
    # two Firestream aliases (--os / --dist) used by our own callers.
    #
    # /etc/os-release is absent in the Nix build sandbox and on some minimal
    # images, so every lookup falls back to `uname` rather than returning empty:
    # a caller asking "what OS is this" should always get an answer.
    #
    # Arguments:
    #   $1 - flag: --id | --version | --branch | --codename | --name
    #             | --pretty-name | --os | --dist
    # Returns:
    #   String
    #########################
    get_os_metadata() {
        local -r flag_name="''${1:?missing flag}"

        _os_release_field() {
            local -r env_name="''${1:?missing environment variable name}"
            if [[ -r /etc/os-release ]]; then
                (
                    . /etc/os-release
                    echo "''${!env_name-}"
                )
            fi
        }

        case "$flag_name" in
            --id)          _os_release_field ID ;;
            --version)     _os_release_field VERSION_ID ;;
            --branch)      _os_release_field VERSION_ID | ${pkgs.gnused}/bin/sed 's/\..*//' ;;
            --codename)    _os_release_field VERSION_CODENAME ;;
            --name)        _os_release_field NAME ;;
            --pretty-name) _os_release_field PRETTY_NAME ;;
            # Kernel/OS family, e.g. "Linux". Always answerable.
            --os)
                ${pkgs.coreutils}/bin/uname -s
                ;;
            # Distribution id, falling back to the kernel name off-distro.
            --dist)
                local dist
                dist="$(_os_release_field ID)"
                if [[ -z "$dist" ]]; then
                    dist="$(${pkgs.coreutils}/bin/uname -s)"
                fi
                echo "$dist"
                ;;
            *)
                error "get_os_metadata: unknown flag ''${flag_name}"
                return 1
                ;;
        esac
    }

    ########################
    # Convert memory string to MB
    # Arguments:
    #   $1 - Memory string (e.g., "2G", "512M", "1024K")
    # Returns:
    #   Memory in MB
    #########################
    convert_to_mb() {
        local mem_str="''${1:?memory string is missing}"
        local number
        local unit

        # Extract number and unit
        number=$(${pkgs.coreutils}/bin/echo "$mem_str" | ${pkgs.gnugrep}/bin/grep -oE '^[0-9]+')
        unit=$(${pkgs.coreutils}/bin/echo "$mem_str" | ${pkgs.gnugrep}/bin/grep -oE '[A-Za-z]+$')

        if [[ -z "$number" ]]; then
            stderr_print "Invalid memory format: $mem_str"
            return 1
        fi

        case "''${unit^^}" in
            G|GB)
                ${pkgs.coreutils}/bin/echo $((number * 1024))
                ;;
            M|MB)
                ${pkgs.coreutils}/bin/echo "$number"
                ;;
            K|KB)
                ${pkgs.coreutils}/bin/echo $((number / 1024))
                ;;
            *)
                # Assume bytes if no unit
                ${pkgs.coreutils}/bin/echo $((number / 1024 / 1024))
                ;;
        esac
    }

    ########################
    # Get machine size classification based on memory
    # Arguments:
    #   None
    # Returns:
    #   Size string (micro/small/medium/large/xlarge/2xlarge)
    #########################
    get_machine_size() {
        local memory_mb
        memory_mb=$(get_total_memory)

        if [[ "$memory_mb" -lt 2048 ]]; then
            ${pkgs.coreutils}/bin/echo "micro"
        elif [[ "$memory_mb" -lt 4096 ]]; then
            ${pkgs.coreutils}/bin/echo "small"
        elif [[ "$memory_mb" -lt 8192 ]]; then
            ${pkgs.coreutils}/bin/echo "medium"
        elif [[ "$memory_mb" -lt 16384 ]]; then
            ${pkgs.coreutils}/bin/echo "large"
        elif [[ "$memory_mb" -lt 32768 ]]; then
            ${pkgs.coreutils}/bin/echo "xlarge"
        else
            ${pkgs.coreutils}/bin/echo "2xlarge"
        fi
    }

    ########################
    # Get machine ID
    # Arguments:
    #   None
    # Returns:
    #   Machine ID string
    #########################
    get_machine_id() {
        if [[ -r /etc/machine-id ]]; then
            ${pkgs.coreutils}/bin/cat /etc/machine-id
        else
            # Fallback to /proc/sys/kernel/random/boot_id
            ${pkgs.coreutils}/bin/cat /proc/sys/kernel/random/boot_id 2>/dev/null || ${pkgs.coreutils}/bin/echo "unknown"
        fi
    }

    ########################
    # Execute command with debug output control
    # Arguments:
    #   Command to execute
    # Globals:
    #   FIRESTREAM_DEBUG
    # Returns:
    #   Command exit code
    #########################
    debug_execute() {
        if [[ "$#" -eq 0 ]]; then
            stderr_print "missing command to execute"
            return 1
        fi

        # Check FIRESTREAM_DEBUG
        local debug_bool="''${FIRESTREAM_DEBUG:-false}"
        shopt -s nocasematch
        if [[ "$debug_bool" = 1 || "$debug_bool" =~ ^(yes|true)$ ]]; then
            "$@"
        else
            "$@" >/dev/null 2>&1
        fi
    }

    ########################
    # Retry command while it fails
    # Arguments:
    #   Command to retry
    # Flags:
    #   --tries - Number of retry attempts (default: 12)
    #   --sleep - Seconds to sleep between retries (default: 5)
    # Returns:
    #   Last command exit code
    #########################
    retry_while() {
        local tries=12
        local sleep_time=5
        local return_value=1
        local -a cmd=()

        # Flags are parsed WHEREVER they appear, not only before the command.
        # Both orders are in use in this tree:
        #   retry_while --tries 60 --sleep 10 is_db_migrated       (flags first)
        #   retry_while "<cmd string>" --tries 30 --sleep 10       (flags last)
        # The old loop `break`-ed at the first non-flag, so in the second shape
        # `--tries 30 --sleep 10` were silently appended to the command instead
        # of parsed — the retry budget was ignored. `--` still ends flag
        # parsing, so a command that genuinely starts with a dash stays callable.
        local end_of_flags=0
        while [[ "$#" -gt 0 ]]; do
            if (( end_of_flags )); then
                cmd+=("$1")
                shift
                continue
            fi
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
                    end_of_flags=1
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    cmd+=("$1")
                    ;;
            esac
            shift
        done

        if [[ "''${#cmd[@]}" -eq 0 ]]; then
            stderr_print "missing command to retry"
            return 1
        fi

        # Two call shapes are in use in this tree and BOTH must work:
        #
        #   retry_while --tries 60 --sleep 10 is_db_migrated
        #       -> argv form: exec the words directly.
        #
        #   retry_while "airflow db check-migrations --timeout=$X" --tries 30
        #       -> single-string form (see src/containers/firestream/airflow/
        #          scripts/init.sh). This was BROKEN: `cmd=("$@")` keeps the
        #          quoted string as one array element, so the exec looked for a
        #          program literally named "airflow db check-migrations ...",
        #          got "command not found" every attempt, and the call could
        #          never succeed. Trailing flags after the string were also
        #          swallowed into cmd rather than parsed.
        #
        # A single argument containing whitespace is therefore treated as a
        # shell snippet and eval'd, which also admits compound conditions like
        # "n=$((n+1)); [ $n -lt 3 ]". Anything else keeps exact argv semantics,
        # so a command whose name contains no space is never re-parsed.
        local use_eval=0
        if [[ "''${#cmd[@]}" -eq 1 && "''${cmd[0]}" =~ [[:space:]] ]]; then
            use_eval=1
        fi

        for ((i = 1; i <= tries; i++)); do
            debug "Attempt $i/$tries: ''${cmd[*]}"
            if (( use_eval )); then
                if eval "''${cmd[0]}"; then
                    return_value=0
                    break
                fi
            elif "''${cmd[@]}"; then
                return_value=0
                break
            fi
            ${pkgs.coreutils}/bin/sleep "$sleep_time"
        done
        return "$return_value"
    }

    ########################
    # Generate random string
    # Arguments:
    #   None
    # Flags:
    #   --type - Type of characters (ascii/numeric/alphanumeric, default: alphanumeric)
    #   --count - Number of characters (default: 32)
    # Returns:
    #   Random string
    #########################
    generate_random_string() {
        local type="alphanumeric"
        local count=32

        # Parse arguments
        while [[ "$#" -gt 0 ]]; do
            case "$1" in
                --type)
                    shift
                    type="''${1:?missing type value}"
                    ;;
                --count)
                    shift
                    count="''${1:?missing count value}"
                    ;;
                -*)
                    stderr_print "unrecognized flag $1"
                    return 1
                    ;;
                *)
                    stderr_print "unexpected argument: $1"
                    return 1
                    ;;
            esac
            shift
        done

        local chars=""
        case "$type" in
            ascii)
                chars='A-Za-z'
                ;;
            numeric)
                chars='0-9'
                ;;
            alphanumeric)
                chars='A-Za-z0-9'
                ;;
            *)
                stderr_print "Invalid type: $type (must be ascii/numeric/alphanumeric)"
                return 1
                ;;
        esac

        ${pkgs.coreutils}/bin/tr -dc "$chars" < /dev/urandom | ${pkgs.coreutils}/bin/head -c "$count"
    }

    ########################
    # Run command as specified user
    # Arguments:
    #   $1 - username
    #   $@ - command to run
    # Returns:
    #   Command exit code
    #########################
    run_as_user() {
        if [[ "$#" -lt 2 ]]; then
            stderr_print "usage: run_as_user USERNAME COMMAND..."
            return 1
        fi

        local run_as_user="''${1}"
        shift

        if am_i_root; then
            debug "Running command as user $run_as_user: $*"
            ${pkgs.bashInteractive}/bin/bash -c "${pkgs.coreutils}/bin/chroot --userspec=\"$run_as_user\" / \"$@\""
        else
            debug "Not running as root, executing command directly: $*"
            "$@"
        fi
    }
  '';
in
{
  meta = {
    name = "libos";
    description = "Operating system utilities and user management";
    version = "1.0.0";
  };

  imports = [ logModule fsModule validationsModule ];
  runtimeDeps = with pkgs; [
    coreutils
    shadow
    gawk
    gnugrep
    glibc.bin
    bashInteractive
  ];
  inherit functions;
  exports = [
    "user_exists"
    "group_exists"
    "ensure_user_exists"
    "ensure_group_exists"
    "am_i_root"
    "get_total_memory"
    "get_total_cpus"
    "get_os_metadata"
    "convert_to_mb"
    "get_machine_size"
    "get_machine_id"
    "debug_execute"
    "retry_while"
    "generate_random_string"
    "run_as_user"
  ];

  script = pkgs.writeTextDir "opt/firestream/scripts/libos.sh" ''
    #!/bin/bash
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.
    ${functions}
  '';
}
