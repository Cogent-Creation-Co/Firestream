#!/usr/bin/env bash
# mix-deps.sh — write mix-deps.nix next to each Elixir app from its mix.lock.
#
# mkMixRelease fetches each Mix dependency on its own, from the mix-deps.nix
# that this script writes (see mix-deps.exs). Run it after every change to
# the source pin of an app below, then `git add` the result. The flake check
# `<app>-mix-deps` runs the same generator offline and fails on a stale file.
#
# Usage: bin/nix/firestream/beam/mix-deps.sh

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
system="$(nix eval --raw --impure --expr builtins.currentSystem)"

# <check name> <app dir>
apps=(
  "supabase-realtime-mix-deps src/containers/firestream/supabase-realtime"
  "supabase-supavisor-mix-deps src/containers/firestream/supavisor"
)

for app in "${apps[@]}"; do
  read -r check dir <<<"$app"
  attr="$root#checks.$system.$check"
  src="$(nix build --no-link --print-out-paths "$attr.lockSource")"
  label="$(nix eval --raw "$attr.lockLabel")"
  out="$root/$dir/mix-deps.nix"
  nix shell --inputs-from "$root" nixpkgs#elixir --command \
    elixir "$root/bin/nix/firestream/beam/mix-deps.exs" \
    --lock "$src/mix.lock" --label "$label" --hashes-from "$out" --prefetch > "$out.new"
  mv "$out.new" "$out"
  echo "wrote $dir/mix-deps.nix"
done
