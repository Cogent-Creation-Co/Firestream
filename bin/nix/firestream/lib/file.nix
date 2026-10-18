# file.nix - File manipulation utilities (ConfigMap-safe)
{ pkgs, lib, logModule }:

let
  functions = ''
    # Import log functions
    ${logModule.functions}

    # ARGUMENT ORDER IS FILENAME-FIRST, matching upstream Bitnami libfile.sh.
    #
    # These three helpers previously took (match, substitution, filename) — the
    # reverse of both upstream and of every caller in this repo
    # (postgresql/module.nix, kafka/scripts/helpers.sh, odoo/libodoo.sh,
    # spark/libspark.sh all call `replace_in_file "$file" "$match" "$sub"`).
    # Called the real way, sed received the substitution as its filename:
    #
    #     sed: can't read listen_addresses = 'NEW': No such file or directory
    #
    # ...and the target file was left untouched, with only a stderr line to show
    # for it. This function is concatenated into EVERY app image via
    # apps/base.nix, so the broken version shipped everywhere; it went unnoticed
    # only because each container defined its own local override on top.
    #
    # The delimiter is now a non-printable character (\001) as upstream does,
    # replacing a scan loop that tested `[[ ! "$match" =~ $d ]]` — a REGEX match
    # against the candidate delimiter, so e.g. "." matched everything and the
    # loop's choice was effectively arbitrary.

    ########################
    # Replace a string in a file (ConfigMap-safe - no sed -i)
    # Arguments:
    #   $1 - filename - File to modify
    #   $2 - match - Regex to search for
    #   $3 - substitution - String to replace with
    #   $4 - posix_regex - Use extended regex (default: true)
    # Returns:
    #   None
    #########################
    replace_in_file() {
        local filename="''${1:?filename is missing}"
        local match="''${2:?match pattern is missing}"
        local substitution="''${3:?substitution string is missing}"
        local posix_regex="''${4:-true}"

        # Non-printable delimiter: cannot collide with anything in the pattern.
        local -r del=$'\001'

        # ConfigMap-safe: read, process, write (no sed -i). In-place edits break
        # on ConfigMap-mounted files.
        local result
        if [[ "$posix_regex" = true ]]; then
            result="$(${pkgs.gnused}/bin/sed -E "s''${del}''${match}''${del}''${substitution}''${del}g" "$filename")"
        else
            result="$(${pkgs.gnused}/bin/sed "s''${del}''${match}''${del}''${substitution}''${del}g" "$filename")"
        fi
        ${pkgs.coreutils}/bin/echo "$result" > "$filename"
    }

    ########################
    # Replace a multiline pattern in a file (ConfigMap-safe)
    # Arguments:
    #   $1 - filename - File to modify
    #   $2 - match - Regex pattern to search for (multiline)
    #   $3 - substitution - String to replace with
    # Returns:
    #   None
    #########################
    replace_in_file_multiline() {
        local filename="''${1:?filename is missing}"
        local match="''${2:?match pattern is missing}"
        local substitution="''${3:?substitution string is missing}"

        # ConfigMap-safe: read entire file, process with perl, write back.
        # The expression previously interpolated as `s<match><sub>g` with NO
        # delimiters at all — never valid perl, so this function could only ever
        # die with a compile error. Delimiters + /smg restored, matching
        # upstream Bitnami libfile.sh.
        local result
        result="$(${pkgs.perl}/bin/perl -pe 'BEGIN{undef $/;} s/'"$match"'/'"$substitution"'/smg' "$filename")"
        ${pkgs.coreutils}/bin/echo "$result" > "$filename"
    }

    ########################
    # Remove lines matching a pattern from a file (ConfigMap-safe)
    # Filename-first, matching upstream Bitnami — see the note above.
    # Arguments:
    #   $1 - filename - File to modify
    #   $2 - pattern - Pattern to match for deletion
    #   $3 - posix_regex - Use extended regex (default: true)
    # Returns:
    #   None
    #########################
    remove_in_file() {
        local filename="''${1:?filename is missing}"
        local pattern="''${2:?pattern is missing}"
        local posix_regex="''${3:-true}"

        # A sed ADDRESS uses `/regex/`; a custom delimiter there needs the
        # `\cREGEXc` form, so unlike the `s///` helpers above this one keeps `/`
        # — exactly as upstream Bitnami libfile.sh does.
        # ConfigMap-safe: read, delete matching lines, write
        local result
        if [[ "$posix_regex" = true ]]; then
            result="$(${pkgs.gnused}/bin/sed -E "/''${pattern}/d" "$filename")"
        else
            result="$(${pkgs.gnused}/bin/sed "/''${pattern}/d" "$filename")"
        fi
        ${pkgs.coreutils}/bin/echo "$result" > "$filename"
    }

    ########################
    # Append content after the last line matching a pattern
    # Filename-first, matching upstream Bitnami and the rest of this module.
    # Arguments:
    #   $1 - filename - File to modify
    #   $2 - pattern - Pattern to match
    #   $3 - content - Content to append after last match
    # Returns:
    #   None
    #########################
    append_file_after_last_match() {
        local filename="''${1:?filename is missing}"
        local pattern="''${2:?pattern is missing}"
        local content="''${3:?content is missing}"

        # Find the line number of the last match
        local last_match_line
        last_match_line=$(${pkgs.gnugrep}/bin/grep -n "$pattern" "$filename" | ${pkgs.coreutils}/bin/tail -1 | ${pkgs.coreutils}/bin/cut -d: -f1)

        if [[ -z "$last_match_line" ]]; then
            warn "Pattern '$pattern' not found in $filename, appending to end of file"
            ${pkgs.coreutils}/bin/echo "$content" >> "$filename"
        else
            # ConfigMap-safe: read, insert after last match, write
            local result
            result="$(${pkgs.gnused}/bin/sed "''${last_match_line}a\\
$content" "$filename")"
            ${pkgs.coreutils}/bin/echo "$result" > "$filename"
        fi
    }

    ########################
    # Wait for a log entry to appear in a file
    # Arguments:
    #   $1 - log_pattern - Pattern to search for in logs
    #   $2 - log_file - Log file to monitor
    #   $3 - timeout - Timeout in seconds (default: 60)
    # Returns:
    #   0 if pattern found, 1 if timeout
    #########################
    wait_for_log_entry() {
        local log_pattern="''${1:?log pattern is missing}"
        local log_file="''${2:?log file is missing}"
        local timeout="''${3:-60}"

        debug "Waiting for log entry matching '$log_pattern' in $log_file (timeout: ''${timeout}s)"

        # Create a unique temporary file for the tail process
        local tail_pid
        local found=0

        # Use timeout command with tail and grep
        if ${pkgs.coreutils}/bin/timeout "$timeout" ${pkgs.coreutils}/bin/tail -f "$log_file" 2>/dev/null | ${pkgs.gnugrep}/bin/grep -q -m 1 "$log_pattern"; then
            info "Log entry found: $log_pattern"
            return 0
        else
            error "Timeout waiting for log entry: $log_pattern"
            return 1
        fi
    }
  '';
in
{
  meta = {
    name = "libfile";
    description = "File manipulation utilities (ConfigMap-safe)";
    version = "1.0.0";
  };

  imports = [ logModule ];
  runtimeDeps = with pkgs; [ coreutils gnused gnugrep perl ];
  inherit functions;
  exports = [
    "replace_in_file"
    "replace_in_file_multiline"
    "remove_in_file"
    "append_file_after_last_match"
    "wait_for_log_entry"
  ];

  script = pkgs.writeTextDir "opt/firestream/scripts/libfile.sh" ''
    #!/bin/bash
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.
    ${functions}
  '';
}
