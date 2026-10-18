#!/usr/bin/env bash
# Reclaim disk on a GitHub-hosted runner before a Nix build.
#
# A hosted `ubuntu-latest` runner ships ~14 GB free on `/`. Firestream's
# checkout alone is >1 GB (the vendored Bitnami chart + container forks), and a
# `verify`-phase Nix store on top of that does not fit without help. The
# pre-installed toolchains below are worth ~25 GB and are used by exactly none
# of Firestream's jobs.
#
# This is the concrete thing `CLAUDE.md` used to describe as "cleans up space by
# removing unnecessary GitHub Actions tools". Every removal is best-effort: a
# missing path on a future runner image must not fail the job.
#
# Kept deliberately as a script rather than a third-party action so there is no
# supply-chain surface on a step that runs with sudo.
set -uo pipefail

echo "── before ──"
df -h / || true

sudo rm -rf \
  /usr/share/dotnet \
  /usr/local/lib/android \
  /opt/ghc \
  /usr/local/.ghcup \
  /usr/local/share/powershell \
  /usr/local/share/chromium \
  /usr/local/lib/node_modules \
  /opt/hostedtoolcache/CodeQL \
  2>/dev/null || true

# The runner's Docker image cache. Firestream's hosted jobs are native-Nix
# (FIRESTREAM_BUILD_STRATEGY=auto resolves to `native` on a Linux host with nix
# on PATH), so nothing here needs the preloaded images. A self-hosted runner
# using STRATEGY=docker should NOT run this script.
if command -v docker >/dev/null 2>&1; then
  sudo docker image prune --all --force >/dev/null 2>&1 || true
fi

sudo apt-get clean || true

echo "── after ──"
df -h / || true
