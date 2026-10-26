---
id: FIRE-047
title: "P5: pinning verification, reproducibility check and Supabase e2e"
type: feature
status: done
priority: high
tags: [supabase, nix, e2e]
affects: []
depends-on: [FIRE-044, FIRE-045, FIRE-046, FIRE-048, FIRE-058]
---

# P5: pinning verification, reproducibility check and Supabase e2e

Epic: [[FIRE-041]]

## Scope

1. Add the P0 scan ([[FIRE-042]]) to the `verify` CI phase, and remove its `TODO(FIRE-047)`.
2. Add a reproducibility phase in the e2e or nightly mode. It runs `nix build --rebuild` on each remaining FOD: the Go vendors, the per-package fetches and the Envoy inputs. Drift then fails at once.
3. Run `make test-e2e-supabase` and `make test-e2e-k8s-supabase` on the new images, with the clean-teardown harness ([[FIRE-034]]).
4. Update `docs/supabase-maintainer.md`: how to bump Envoy (regenerate `deps.json`), the Mix deps and the Node source builds.

## Acceptance

- The scan passes for all ten images in `verify`.
- The reproducibility pass gives the same hash for every FOD.
- Both Supabase e2e suites pass, and the host snapshot is unchanged after each run.
- `make ci-dry-run`, `make ci-e2e-dry-run` and `nix flake check --no-build --all-systems` pass.

## Work log

### 2026-10-23

Step 1 done: the scan is in `verify`.
- `profile.nix`: the `supabase-no-prebuilt-binaries` entry and its `TODO(FIRE-047)` are gone from `excludedChecks`.
- `make ci-dry-run` (in `nix develop`) lists `[required] checks.x86_64-linux.supabase-no-prebuilt-binaries` under `verify`.
- `nix build .#checks.x86_64-linux.supabase-no-prebuilt-binaries` passes.
- `docs/nix-pinning-policy.md`: the CI status section now tells that `verify` gates the check.

### 2026-10-23

Steps 2 and 3: the reproducibility phase and the docs.
- New script `bin/nix/firestream/supabase/rebuild-fods.py`. It reads the drv closure of each image with `nix derivation show --recursive`. It keeps each FOD, and drops each FOD whose output cache.nixos.org serves (the trusted base). Then it runs `nix build` and `nix build --rebuild` in batches of 200. No IFD.
- Count: 7499 FODs in the closure of the ten images. 3472 are not on cache.nixos.org: 2692 npm tarballs, 268 cargo crates, 476 Hex, Envoy, Go module and fenix files, 29 GitHub sources, 7 vendor FODs.
- Negative test: a FOD from a local HTTP server, then a changed file. The script prints `DRIFT <drv>` and exits 1. I deleted the test store paths.
- `profile.nix`: new advisory phase `repro-supabase-fods`, mode `e2e`, at the head of the chain. The image list comes from `containerPackages`.
- `make ci-e2e-dry-run` shows the phase. `cargo test -p firestream-ci` passes, with the 12 `e2e_phase_chain` tests on the ambient profile.
- `nix flake check --no-build --all-systems` passes.
- Docs: `docs/supabase-maintainer.md` has new sections for Envoy, Mix deps, pnpm lockfiles, native Node tools, the scan allowlist and the reproducibility check. CLAUDE.md names the new phase.
- The full local run (3472 FODs) is in progress.

### 2026-10-23

Full reproducibility run done. It ran on all 3472 FODs, not on a subset, in about 15 minutes.
- Result: `reproduced 3472/3472`. Nix checked each output again ("checking outputs of", 3472 lines). There was no hash mismatch and no failed fetch.
- The seven vendor FODs reproduced: the Auth and imgproxy Go modules, and the cargo vendors of the Supavisor NIF, oxfmt, oxide, lightningcss and next-swc.
- Gap: the script does not see fetches at evaluation time, for example `builtins.fetchurl` of the Rust channel manifests in the native Node tools. Nix checks those hashes at each evaluation.
- Next: wait for FIRE-037, then run the two Supabase e2e suites.

### 2026-10-23

Step 4 waits on FIRE-058 (the supabase-postgres readiness defect). The PM gave this order.
- I started `make test-e2e-supabase` before the PM note arrived. I stopped it during `nix develop` evaluation, before the harness started.
- The host snapshot (`docker ps -a`, `docker network ls`, `docker volume ls`, `k3d cluster list`) is the same before and after the stop.
- Scratch files and snapshot script: `~/.cache/fire-047/`.
- Still to do: run both Supabase e2e suites with snapshots after FIRE-058 is done. Then set FIRE-047 and FIRE-041 to `done`.
- Git-added: `bin/nix/firestream/ci/profile.nix`, `bin/nix/firestream/supabase/rebuild-fods.py`, `docs/supabase-maintainer.md`, `docs/nix-pinning-policy.md`, `CLAUDE.md`.

### 2026-10-23

Step 4 done. FIRE-058 is done, so the e2e runs went ahead.
- `make test-e2e-supabase` (STRICT=1): all probes passed (project `firestream-e2e-supabase-yviflo`). The teardown ran `supabase-down -v`.
- `make test-e2e-k8s-supabase` (K8S_STRICT=1): all probes passed, including storage, realtime, supavisor and studio.
- The host snapshots (`docker ps -a`, `docker network ls`, `docker volume ls`, `k3d cluster list`) are identical before and after each run.

Ticket done.

Files (git-added):
- `bin/nix/firestream/ci/profile.nix`: the scan is in `verify`, and the phase `repro-supabase-fods` heads the e2e chain.
- `bin/nix/firestream/supabase/rebuild-fods.py`: new.
- `docs/supabase-maintainer.md`, `docs/nix-pinning-policy.md`, `CLAUDE.md`.

Verification:
- `make ci-dry-run` lists the scan under `verify`. The scan check builds and passes.
- `make ci-e2e-dry-run` shows the new phase. `cargo test -p firestream-ci` passes.
- The rebuild gives the same hash for 3472 of 3472 FODs. A negative test reports `DRIFT`.
- `nix flake check --no-build --all-systems` passes.
- Both Supabase e2e suites pass, with clean snapshots.

Deviations:
- The script drops each FOD that cache.nixos.org serves, because the policy trusts that cache.
- The script does not see fetches at evaluation time, such as the Rust channel manifests.
- I did not run the full `make ci-check`. I built the scan check only.
