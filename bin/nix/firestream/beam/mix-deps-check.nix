# mix-deps check builder
# Copyright Firestream. MIT License.
#
# Returns a check that runs mix-deps.exs offline on the mix.lock of a
# release and compares the result with the committed mix-deps.nix. The
# release must come from mkMixRelease with `mixDeps` set.
#
# Usage (from a flake-module):
#   checks.supabase-realtime-mix-deps = import .../mix-deps-check.nix { inherit pkgs; } {
#     name = "supabase-realtime-mix-deps"; release = ...; path = "src/containers/...";
#   };

{ pkgs }:

{ name, release, path }:

let
  inherit (release) src;
  label = "${src.owner}/${src.repo}";
  committed = release.mixDepsFile;
in
pkgs.runCommand name
{
  nativeBuildInputs = [ pkgs.elixir ];
  LANG = "C.UTF-8";
  passthru = {
    lockSource = src;
    lockLabel = label;
  };
} ''
  elixir ${./mix-deps.exs} --lock ${src}/mix.lock --label ${pkgs.lib.escapeShellArg label} \
    --hashes-from ${committed} > generated.nix
  if ! diff -u ${committed} generated.nix; then
    echo "FAIL: ${path}/mix-deps.nix is stale; run bin/nix/firestream/beam/mix-deps.sh"
    exit 1
  fi
  echo "PASS: ${path}/mix-deps.nix matches the mix.lock of ${label}"
  touch "$out"
''
