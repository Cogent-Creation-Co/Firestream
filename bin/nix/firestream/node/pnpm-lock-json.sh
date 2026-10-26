#!/usr/bin/env bash
# pnpm-lock-json.sh — write pnpm-lock.json next to each pnpm-lock.yaml.
#
# The vendored pnpm2nix (bin/nix/firestream/node/pnpm2nix.nix) reads the
# JSON form of a pnpm lockfile, because a YAML parse at eval time needs
# import-from-derivation. Run this script after every change to a
# pnpm-lock.yaml under src/containers/firestream, then `git add` both files.
# The flake check `pnpm-lock-json` runs the same conversion and fails on a
# stale JSON file.
#
# Usage: bin/nix/firestream/node/pnpm-lock-json.sh

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"

# WARNING: the check compares bytes. A yaml2json from another remarshal
# release, or other flags, writes JSON that the check rejects.
if [[ "${PNPM_LOCK_JSON_IN_SHELL:-}" != 1 ]]; then
  PNPM_LOCK_JSON_IN_SHELL=1 exec nix shell --inputs-from "$root" nixpkgs#remarshal \
    --command "${BASH_SOURCE[0]}" "$@"
fi

find "$root/src/containers/firestream" -mindepth 2 -maxdepth 3 -name pnpm-lock.yaml -print0 | sort -z |
  while IFS= read -r -d '' yaml; do
    yaml2json --indent 2 "$yaml" "${yaml%.yaml}.json"
    echo "wrote ${yaml#"$root"/} -> pnpm-lock.json"
  done
