#!/usr/bin/env bash
# test-registry-parity.sh — the shell half of the container-registry parity gate.
#
# Sister to test-strategy-parity.sh. Where that file gates the native-vs-docker
# PREDICATE, this one gates the container -> Nix package-name TABLE, which is
# the other thing the Rust build path has to reproduce byte for byte.
#
# It asserts three things against bin/build/registry-cases.json:
#
#   1. Every key in `entries` exists in _common.sh's CONTAINER_REGISTRY with
#      the same value.
#   2. Every key in CONTAINER_REGISTRY exists in `entries` with the same value.
#      (Both directions, so neither side can grow a key unnoticed.)
#   3. Every vector in `cases` resolves through the REAL resolve_package_name
#      to the expected package — including the failure vectors, where a null
#      `expect` must produce a non-zero exit.
#
# The Rust half is `firestream_ci::profile::spec`'s registry tests plus
# `firestream-ci build resolve`, which read the SAME table out of
# ci-manifest.json (bin/nix/firestream/ci/profile.nix imports this JSON
# verbatim). No package table is compiled into Rust.
#
# Field separator is US (0x1f), NOT tab: tab is IFS whitespace, so bash's
# `read` would collapse the empty `version` field of every default-version
# vector and silently test the wrong thing.
#
# Usage: bash bin/build/test-registry-parity.sh [path/to/registry-cases.json]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
CASES="${1:-${FIRESTREAM_REGISTRY_CASES:-$SCRIPT_DIR/registry-cases.json}}"
COMMON_SH="${FIRESTREAM_COMMON_SH:-$SCRIPT_DIR/_common.sh}"

[[ -f "$CASES"     ]] || { echo "no cases file at $CASES" >&2; exit 1; }
[[ -f "$COMMON_SH" ]] || { echo "no _common.sh at $COMMON_SH" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

PASS=0; FAIL=0
declare -a FAILURES=()

ok()   { PASS=$((PASS + 1)); }
bad()  { FAIL=$((FAIL + 1)); FAILURES+=("$1"); printf '  FAIL  %s\n' "$1"; }

# _common.sh runs `set -euo pipefail` and resolves REPO_ROOT at source time;
# both are fine here since we are inside the repo.
# shellcheck source=/dev/null
source "$COMMON_SH"

echo "registry parity: $CASES"
echo "           vs.: $COMMON_SH"
echo

# ── 1: every golden entry is in the bash table ───────────────────────
while IFS=$'\x1f' read -r key want; do
    got="${CONTAINER_REGISTRY[$key]:-}"
    if [[ "$got" == "$want" ]]; then
        ok
    elif [[ -z "$got" ]]; then
        bad "entries['$key'] = '$want' but CONTAINER_REGISTRY has no such key"
    else
        bad "entries['$key'] = '$want' but CONTAINER_REGISTRY['$key'] = '$got'"
    fi
done < <(jq -r '.entries | to_entries[] | "\(.key)\(.value)"' "$CASES")

# ── 2: and the reverse, so neither side can grow a key unnoticed ─────
for key in "${!CONTAINER_REGISTRY[@]}"; do
    want="${CONTAINER_REGISTRY[$key]}"
    got="$(jq -r --arg k "$key" '.entries[$k] // "@@ABSENT@@"' "$CASES")"
    if [[ "$got" == "$want" ]]; then
        ok
    elif [[ "$got" == "@@ABSENT@@" ]]; then
        bad "CONTAINER_REGISTRY['$key'] = '$want' but registry-cases.json has no such key"
    else
        bad "CONTAINER_REGISTRY['$key'] = '$want' but entries['$key'] = '$got'"
    fi
done

# ── 3: the resolution vectors ────────────────────────────────────────
while IFS=$'\x1f' read -r container version want; do
    label="resolve_package_name($container, '${version}')"
    if got="$(resolve_package_name "$container" "$version" 2>/dev/null)"; then
        rc=0
    else
        rc=1
        got=""
    fi

    if [[ "$want" == "@@ABSENT@@" ]]; then
        if [[ $rc -ne 0 ]]; then ok; else bad "$label expected FAILURE, got '$got'"; fi
    elif [[ $rc -ne 0 ]]; then
        bad "$label expected '$want', but resolve_package_name exited non-zero"
    elif [[ "$got" == "$want" ]]; then
        ok
    else
        bad "$label expected '$want', got '$got'"
    fi
done < <(jq -r '.cases[] | "\(.container)\(.version)\(.expect // "@@ABSENT@@")"' "$CASES")

echo
echo "  passed: $PASS"
echo "  failed: $FAIL"
if [[ $FAIL -gt 0 ]]; then
    echo
    echo "FAILURES:"
    printf '  - %s\n' "${FAILURES[@]}"
    exit 1
fi
echo "registry parity OK"
