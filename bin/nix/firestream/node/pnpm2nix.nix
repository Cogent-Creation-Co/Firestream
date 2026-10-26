# pnpm2nix builder for pnpm-locked Node.js applications
# Copyright Firestream. MIT License.
#
# Returns { mkPnpmPackage } from the pnpm2nix copy in ./pnpm2nix (upstream
# github:FliegendeWurst/pnpm2nix-nzbr at
# 9b15300b711a50bb96e553b66aad5cad94df9c7e, ISC). Every locked package is
# its own fixed-output fetch (integrity from the lockfile), so a lockfile
# change refetches only the changed packages. The install runs
# `pnpm install --ignore-scripts` against a store filled from those fetches,
# so no install script runs: the caller builds each native module from
# source.
#
# NOTE: the FliegendeWurst fork, because nzbr/pnpm2nix-nzbr reads neither
# pnpm lockfile v9 nor pnpm workspaces.
#
# The copy differs from upstream in three ways:
#   - the platform filter reads negated entries (`os: ["!win32"]`) and the
#     `libc` field, and drops a filtered package from dependency maps that
#     carry a peer suffix. Without this, pnpm 10 and 11 stop with "Cannot use
#     'in' operator to search for 'directory' in undefined".
#   - it reads `pnpmLockJson`, the JSON form of pnpm-lock.yaml, which
#     bin/nix/firestream/node/pnpm-lock-json.sh writes next to the YAML.
#     The flake check `pnpm-lock-json` fails when the two differ.
#   - `excludePackages` drops the named packages the way the platform filter
#     drops a package of another platform, so a prebuilt native package that
#     the caller builds from source never enters the pnpm store.
#
# Usage (from a flake-module):
#   pnpm2nix = import ../../../bin/nix/firestream/node/pnpm2nix.nix { inherit pkgs; };
#   app = pnpm2nix.mkPnpmPackage {
#     pname = ...; src = ...; pnpmLockJson = ./pnpm-lock.json; ...
#   };

{ pkgs }:

pkgs.callPackage ./pnpm2nix/derivation.nix { }
