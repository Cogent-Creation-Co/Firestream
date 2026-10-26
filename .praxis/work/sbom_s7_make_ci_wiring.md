---
id: FIRE-056
title: "S7: wire the SBOM and Dependabot into make ci, the PR gate and CLAUDE.md"
type: feature
status: backlog
priority: high
tags: [dependabot, sbom, ci, docs]
affects: []
depends-on: [FIRE-052, FIRE-054, FIRE-055]
---

# S7: wire the SBOM and Dependabot into make ci, the PR gate and CLAUDE.md

Epic: [[FIRE-049]]

## Scope

1. In `makefile`, add a `dependabot` target:
   ```make
   .PHONY: dependabot
   dependabot:
   	@out=$$(nix build .#dependabot-config --no-link --print-out-paths) && install -m 0644 "$$out" .github/dependabot.yml
   ```
   Make `ci` and `ci-check` depend on `dependabot`. Add `dependabot` and the coverage check to the help text near line 60.
2. In `.github/workflows/build.yaml`, add one step to the hosted `ci-profile` job. It builds `.#checks.x86_64-linux.firestream-dependabot-config` and `.#checks.x86_64-linux.firestream-sbom-coverage`. Then PRs catch drift without `FIRESTREAM_CI_RUNNER`.
3. The flake sees only tracked files. Stage the new files with `git add`: `.github/dependabot.yml`, the new `.nix` files and the new Rust modules.
4. In `CLAUDE.md`, add a short "SBOM and Dependabot" section in STE. It covers the `sbomInputs` rule, `make dependabot`, the two checks and the rule that no in-repo lockfile has a hash.

## Acceptance

- `make ci-dry-run` lists 27 images under `build`, and both new checks under `verify`.
- `make ci-check` exits 0.
- `make manifest` and `make manifest-validate` pass on the full fleet.
