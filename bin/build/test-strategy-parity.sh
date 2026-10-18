#!/usr/bin/env bash
# test-strategy-parity.sh — the shell half of the strategy parity gate.
#
# Drives every vector in bin/build/strategy-cases.json through the REAL
# bin/build/strategy.sh and asserts the strategy + the verbatim blocker string.
# The Rust half (src/util/firestream-ci/src/platform/parity.rs) drives the same
# file through `Probe` fixtures. Divergence between the two is a failure.
#
# ── HOW THE PROBE CONDITIONS ARE INJECTED ────────────────────────────
# strategy.sh probes the real host. Each case therefore runs in a fresh
# `env -i` bash with:
#
#   uname -s / uname -m   a fake `uname` executable in a sandbox bin dir which
#                         is the ENTIRE PATH. strategy.sh calls the real
#                         `uname` command, unmodified.
#   nix on PATH           a fake `nix` executable dropped into that same dir
#                         (or not). `command -v nix` is exercised for real.
#   /nix/store            }
#   /.dockerenv           }  FIRESTREAM_PROBE_ROOT, the one test seam in
#   /proc/1/cgroup        }  strategy.sh: an empty-in-production prefix on
#                            exactly these three absolute paths. They cannot
#                            be injected otherwise without root or a mount
#                            namespace. The Rust `Probe::detect` honours the
#                            same variable, so the two stay mirrors.
#   $KUBERNETES_SERVICE_HOST / $CONTAINER / $container / $FIRESTREAM_BUILD_STRATEGY
#                         plain env, set (possibly to "") via `env -i`, so
#                         "unset" and "empty" are genuinely distinguishable.
#
# Nothing degrades to a no-op: if the seam or the fake PATH stopped working,
# the corresponding cases would read the real host (which HAS /nix/store, HAS
# nix, and is Linux x86_64) and fail loudly.
#
# ── NOT INJECTABLE HERE ──────────────────────────────────────────────
# `profile_default` (ci-manifest.json `build_strategy.default`) is Rust-only —
# strategy.sh is deliberately profile-blind. Cases that set it carry an
# explicit `expect_shell` recording the shell's answer, and this harness
# REFUSES an `expect_shell` on any case whose profile_default is absent or
# "auto", so the field cannot be used to hide real drift.
#
# Usage: bash bin/build/test-strategy-parity.sh [path/to/strategy-cases.json]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
STRATEGY_SH="${FIRESTREAM_STRATEGY_SH:-$SCRIPT_DIR/strategy.sh}"
CASES="${1:-${FIRESTREAM_STRATEGY_CASES:-$SCRIPT_DIR/strategy-cases.json}}"

[[ -f "$STRATEGY_SH" ]] || { echo "no strategy.sh at $STRATEGY_SH" >&2; exit 1; }
[[ -f "$CASES"       ]] || { echo "no cases file at $CASES" >&2; exit 1; }

BASH_BIN="$(type -P bash || true)"; : "${BASH_BIN:=/bin/bash}"
GREP_BIN="$(type -P grep || true)"; : "${GREP_BIN:=/bin/grep}"
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

PASS=0; FAIL=0
declare -a FAILURES=()

TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

# ── the in-sandbox driver ────────────────────────────────────────────
DRIVER="$TMPROOT/driver.sh"
cat >"$DRIVER" <<'DRIVER_EOF'
set -u
# shellcheck disable=SC1090
source "$1"
target="$2"
blocker="$(fs_native_blocker "$target")"
strategy="$(fs_choose_strategy "$target")"
printf 'BLOCKER=%s\n' "$blocker"
printf 'STRATEGY=%s\n' "$strategy"
DRIVER_EOF

ARCH_DRIVER="$TMPROOT/arch-driver.sh"
cat >"$ARCH_DRIVER" <<'ARCH_EOF'
set -u
# shellcheck disable=SC1090
source "$1"
printf 'NORM=%s\n'   "$(fs_norm_arch      "$2")"
printf 'VOLUME=%s\n' "$(get_nix_volume    "$2")"
printf 'PLAT=%s\n'   "$(fs_docker_platform "$2")"
ARCH_EOF

ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILURES+=("$1"); printf '  FAIL %s\n' "$1" >&2; }

cmp_field() { # name expected actual  -> 0/1
    if [[ "$2" == "$3" ]]; then return 0; fi
    printf '         %-9s expected %q\n                   actual   %q\n' "$1:" "$2" "$3" >&2
    return 1
}

# ── build a sandbox for one case ─────────────────────────────────────
# $1 dir  $2 uname_s  $3 uname_m  $4 nix_on_path  $5 nix_store  $6 dockerenv  $7 cgroup(or __NULL__)
make_sandbox() {
    local dir="$1" nix_on_path="$4" nix_store="$5" dockerenv="$6" cgroup="$7"
    mkdir -p "$dir/bin" "$dir/root"

    cat >"$dir/bin/uname" <<EOF
#!$BASH_BIN
case "\${1:-}" in
    -s) printf '%s\n' "\$FS_FAKE_UNAME_S" ;;
    -m) printf '%s\n' "\$FS_FAKE_UNAME_M" ;;
    *)  printf '%s\n' "\$FS_FAKE_UNAME_S" ;;
esac
EOF
    chmod +x "$dir/bin/uname"
    ln -sf "$GREP_BIN" "$dir/bin/grep"

    if [[ "$nix_on_path" == "true" ]]; then
        printf '#!%s\nexit 0\n' "$BASH_BIN" >"$dir/bin/nix"
        chmod +x "$dir/bin/nix"
    fi
    [[ "$nix_store" == "true" ]] && mkdir -p "$dir/root/nix/store"
    [[ "$dockerenv" == "true" ]] && : >"$dir/root/.dockerenv"
    if [[ "$cgroup" != "__NULL__" ]]; then
        mkdir -p "$dir/root/proc/1"
        printf '%s' "$cgroup" >"$dir/root/proc/1/cgroup"
    fi
}

# ── preflight: prove the seams actually bite ─────────────────────────
# If the fake PATH or FIRESTREAM_PROBE_ROOT were being ignored, these two
# assertions would read the real host (Linux, nix present, /nix/store present)
# and the whole suite would be vacuous.
preflight() {
    local d="$TMPROOT/preflight" out
    make_sandbox "$d" x x true false false __NULL__
    out="$(env -i PATH="$d/bin" FIRESTREAM_PROBE_ROOT="$d/root" \
        FS_FAKE_UNAME_S=Linux FS_FAKE_UNAME_M=x86_64 \
        "$BASH_BIN" "$DRIVER" "$STRATEGY_SH" x86_64 2>/dev/null)"
    if [[ "$out" != *"BLOCKER=/nix/store does not exist on this host"* ]]; then
        echo "PREFLIGHT FAILED: FIRESTREAM_PROBE_ROOT seam is not being honoured by $STRATEGY_SH" >&2
        echo "$out" >&2
        exit 1
    fi
    rm -rf "$d"; make_sandbox "$d" x x false true false __NULL__
    out="$(env -i PATH="$d/bin" FIRESTREAM_PROBE_ROOT="$d/root" \
        FS_FAKE_UNAME_S=Darwin FS_FAKE_UNAME_M=arm64 \
        "$BASH_BIN" "$DRIVER" "$STRATEGY_SH" arm64 2>/dev/null)"
    if [[ "$out" != *"uname -s = Darwin"* ]]; then
        echo "PREFLIGHT FAILED: the fake uname on PATH is not being used by $STRATEGY_SH" >&2
        echo "$out" >&2
        exit 1
    fi
    rm -rf "$d"
    echo "preflight: probe seams verified (fake uname on PATH, FIRESTREAM_PROBE_ROOT)"
}
preflight

# ── arch helper cases ────────────────────────────────────────────────
echo
echo "arch_cases (fs_norm_arch / get_nix_volume / fs_docker_platform)"
n_arch="$(jq '.arch_cases | length' "$CASES")"
for ((i=0; i<n_arch; i++)); do
    input="$(jq -r ".arch_cases[$i].input" "$CASES")"
    e_norm="$(jq -r ".arch_cases[$i].norm" "$CASES")"
    e_vol="$(jq -r ".arch_cases[$i].nix_volume" "$CASES")"
    e_plat="$(jq -r ".arch_cases[$i].docker_platform" "$CASES")"

    d="$TMPROOT/arch-$i"; make_sandbox "$d" x x false false false __NULL__
    out="$(env -i PATH="$d/bin" FIRESTREAM_PROBE_ROOT="$d/root" \
        FS_FAKE_UNAME_S=Linux FS_FAKE_UNAME_M=x86_64 \
        "$BASH_BIN" "$ARCH_DRIVER" "$STRATEGY_SH" "$input" 2>/dev/null)"
    a_norm="${out#*NORM=}"; a_norm="${a_norm%%$'\n'*}"
    a_vol="${out#*VOLUME=}"; a_vol="${a_vol%%$'\n'*}"
    a_plat="${out#*PLAT=}";  a_plat="${a_plat%%$'\n'*}"

    rc=0
    cmp_field norm     "$e_norm" "$a_norm" || rc=1
    cmp_field volume   "$e_vol"  "$a_vol"  || rc=1
    cmp_field platform "$e_plat" "$a_plat" || rc=1
    if [[ $rc -eq 0 ]]; then ok "arch:$input"; else bad "arch:$input"; fi
done

# ── strategy cases ───────────────────────────────────────────────────
echo
echo "cases (fs_native_blocker / fs_choose_strategy)"
n="$(jq '.cases | length' "$CASES")"
for ((i=0; i<n; i++)); do
    name="$(jq -r ".cases[$i].name" "$CASES")"
    uname_s="$(jq -r ".cases[$i].probe.uname_s" "$CASES")"
    uname_m="$(jq -r ".cases[$i].probe.uname_m" "$CASES")"
    nix_on_path="$(jq -r ".cases[$i].probe.nix_on_path" "$CASES")"
    nix_store="$(jq -r ".cases[$i].probe.nix_store_present" "$CASES")"
    dockerenv="$(jq -r ".cases[$i].probe.dockerenv_file" "$CASES")"
    cgroup="$(jq -r ".cases[$i].probe.cgroup // \"__NULL__\"" "$CASES")"
    target="$(jq -r ".cases[$i].target_arch" "$CASES")"
    prof="$(jq -r ".cases[$i].profile_default // \"\"" "$CASES")"
    has_shell="$(jq -r "if (.cases[$i] | has(\"expect_shell\")) then \"yes\" else \"no\" end" "$CASES")"

    # Guard: expect_shell may exist ONLY where the profile genuinely diverges.
    if [[ "$has_shell" == "yes" && ( -z "$prof" || "$prof" == "auto" ) ]]; then
        bad "$name (illegal expect_shell: only legal when profile_default is set and != auto)"
        continue
    fi

    if [[ "$has_shell" == "yes" ]]; then
        sel=".cases[$i].expect_shell"
    else
        sel=".cases[$i].expect"
    fi
    e_blocker="$(jq -r "$sel.blocker // \"\"" "$CASES")"
    e_strategy="$(jq -r "$sel.strategy" "$CASES")"
    # `warns` on `expect` describes the FIRESTREAM_BUILD_STRATEGY warning, which
    # the shell also emits; a warn caused solely by a bad profile value is Rust-only.
    e_warns="$(jq -r ".cases[$i].expect.warns // false" "$CASES")"
    if [[ "$has_shell" == "yes" ]]; then
        env_val="$(jq -r ".cases[$i].env.FIRESTREAM_BUILD_STRATEGY // \"__UNSET__\"" "$CASES")"
        case "$env_val" in
            __UNSET__|auto|""|native|docker) e_warns=false ;;
        esac
    fi

    mapfile -t env_pairs < <(jq -r ".cases[$i].env | to_entries[] | \"\(.key)=\(.value)\"" "$CASES")

    d="$TMPROOT/case-$i"
    make_sandbox "$d" "$uname_s" "$uname_m" "$nix_on_path" "$nix_store" "$dockerenv" "$cgroup"

    errf="$d/stderr"
    set +e
    out="$(env -i \
        PATH="$d/bin" \
        FIRESTREAM_PROBE_ROOT="$d/root" \
        FS_FAKE_UNAME_S="$uname_s" \
        FS_FAKE_UNAME_M="$uname_m" \
        "${env_pairs[@]}" \
        "$BASH_BIN" "$DRIVER" "$STRATEGY_SH" "$target" 2>"$errf")"
    rc_run=$?
    set -e
    if [[ $rc_run -ne 0 ]]; then
        bad "$name (driver exited $rc_run)"; cat "$errf" >&2; continue
    fi

    a_blocker="${out#*BLOCKER=}";  a_blocker="${a_blocker%%$'\n'*}"
    a_strategy="${out#*STRATEGY=}"; a_strategy="${a_strategy%%$'\n'*}"
    a_warns=false
    if "$GREP_BIN" -q "Unknown FIRESTREAM_BUILD_STRATEGY" "$errf" 2>/dev/null; then a_warns=true; fi

    rc=0
    cmp_field blocker  "$e_blocker"  "$a_blocker"  || rc=1
    cmp_field strategy "$e_strategy" "$a_strategy" || rc=1
    cmp_field warns    "$e_warns"    "$a_warns"    || rc=1
    if [[ $rc -eq 0 ]]; then ok "$name"; else bad "$name"; fi
done

echo
echo "───────────────────────────────────────────────"
printf 'strategy parity (shell): %d passed, %d failed\n' "$PASS" "$FAIL"
if [[ $FAIL -gt 0 ]]; then
    printf 'failed cases:\n'
    printf '  - %s\n' "${FAILURES[@]}"
    exit 1
fi
exit 0
