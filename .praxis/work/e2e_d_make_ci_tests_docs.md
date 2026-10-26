---
id: FIRE-039
title: "E2E-D: make targets, CI, cleanup tests and docs"
type: feature
status: done
priority: high
tags: [e2e]
affects: []
depends-on: [FIRE-036, FIRE-037, FIRE-038, FIRE-040, FIRE-047]
---

# E2E-D: make targets, CI, cleanup tests and docs

Epic: [[FIRE-034]]

## Scope

1. **Make targets.**
   - `make e2e-sweep` runs the sweeper; `RUN=<id>` and `DRY_RUN=1` are optional.
   - `test-e2e`, `test-e2e-<stack>`, `test-e2e-k8s` and `test-e2e-k8s-<chart>` sweep before the tests.
   - The target names and the env contract do not change.
2. **CI.** The e2e chain in `bin/nix/firestream/ci/profile.nix` runs through these targets, so every link sweeps first. Confirm this with `make ci-e2e-dry-run`, and keep the 12 `e2e_phase_chain` tests green.
3. **Cleanup tests.**
   - Interrupt tests (`#[ignore]`): start a child harness run, send SIGTERM during setup, and assert that no resource of that run ID is left. One case for k3d, one for compose.
   - The lifecycle tests (`e2e_k8s_cluster_lifecycle` and one Docker stack) assert that nothing is left.
   - Prove the leak detection works: a planted labelled resource must make the check fail.
4. **Docs.** Update the CLAUDE.md e2e sections and `docs/supabase.md` with setup, teardown, the lock, the sweep, KEEP=1 and the leak check.

## Acceptance (the epic's verification)

- **Baseline:** take a snapshot of the docker objects, the k3d clusters and the kubeconfig contexts.
- **Supabase:** `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass twice, and the snapshot equals the baseline after each run.
- **Full sweeps:** `make test-e2e` and `make test-e2e-k8s` pass, the snapshot equals the baseline, and no dangling `firestream-*` image is left.
- **Interrupts:** the SIGTERM tests pass, and a manual interrupt leaves nothing.
- **Dev stack:** a running `nix run .#supabase-up` dev stack survives `make e2e-sweep`.
- **KEEP=1:** the resources stay, and `make e2e-sweep RUN=<id>` then removes them all.
- **Unit and CI:** `cargo test` passes for the touched crates and for `firestream-ci`. `make ci-e2e-dry-run` and `nix flake check --no-build --all-systems` pass.

## Work log

### 2026-10-23

Work starts. Scope: make targets, CI chain, cleanup tests, docs and the epic acceptance.

### 2026-10-23

Make targets done. `make e2e-sweep` takes `RUN=<id>` and `DRY_RUN=1`. Every `test-e2e*` target runs `e2e-presweep` first. Outside the devshell the sweep enters `nix develop`.

New tests: `firestream-e2e-core/tests/leak_detection.rs` (planted network and volume fail the check until swept, pass). New module `interrupt.rs` in the core. New `firestream-e2e-k8s/tests/e2e_k8s_interrupt.rs`.

Defect found by the k3d interrupt test (run 3wyf95): the drain raced the live `k3d cluster create`. The child also exited 101 before the drain ended. A network and a volume leaked.

Fix in `cleanup.rs`: the drain first stops every subprocess of the process (SIGTERM, then SIGKILL after 10 s). A guard thread parks while a drain runs. Rerun (j5ubrh): exit 143, nothing left at exit or after 30 s. The host snapshot equals the baseline. `cargo test -p firestream-e2e-core`: 74 pass.

### 2026-10-23

Compose interrupt test added: `firestream/tests/e2e_interrupt.rs` (redis). The first run signalled after setup, because one leak-check pass takes 2.5 s. The trigger now polls docker names. Rerun (ow1o6w): SIGTERM during "Container Creating", exit 143, nothing left.

New target `make test-e2e-cleanup` runs the three cleanup tests. `E2E_DEVSHELL` also skips `nix develop` when k3d is on PATH.

CI: `make ci-e2e-dry-run` passes. All 22 e2e links run `make test-e2e-*`, so each sweeps first. `repro-supabase-fods` still heads the chain. `cargo test -p firestream-ci` passes, 12 `e2e_phase_chain` tests included.

Docs: new CLAUDE.md section "E2E setup, teardown and leak check". New `docs/supabase.md` section "Test the stack".

`nix flake check --no-build --all-systems` passes. Unit tests pass for `firestream-e2e-core` (74), `firestream-e2e-k8s` and `k8s-manager`.

### 2026-10-23

Supabase acceptance, first pass:
- `make test-e2e-supabase` run 1 (ltivqx): pass.
- `make test-e2e-k8s-supabase` runs 1 and 2: pass, 14 tests each.
- `make test-e2e-supabase` run 2 (x1pcoi): fail in the port preflight. Port 50000 was held outside docker and was free a moment later.

Cause: the Supabase host ports (47432, 48543, 50000, 51180) are in the Linux ephemeral range 32768-60999. An outgoing socket can hold one for a short time. Fix in `firestream/tests/e2e/harness.rs`: a holder outside docker gets 20 s of grace. A container holder still fails at once.

Snapshot finding: the devshell docker CLI 29.6.1 lists 11 more untagged ids than the host CLI 28.5.2. The ids are the same in every run. With the host CLI, the host equals the baseline. The snapshot script now uses the host CLI.

### 2026-10-23

Acceptance, second pass:
- `make test-e2e-supabase` runs 3 (2qpeer) and 4 (6shxgt) pass. Each snapshot equals the baseline.
- KEEP=1, compose redis (ozrf9e): the objects stay. `make e2e-sweep RUN=ozrf9e` removes them, and the host equals the baseline.
- KEEP=1, k3d redis plus lifecycle (yfhyhc): 2 clusters, volumes and kubeconfig files stay. `make e2e-sweep RUN=yfhyhc` removes them, and the host equals the baseline.
- Dev stack: `nix run .#supabase-up` starts 11 containers. `make e2e-sweep` finds nothing, and the dev containers and volumes do not change. `nix run .#supabase-down -- -v` then restores the baseline.

Next: `make test-e2e-cleanup`, then `make test-e2e` and `make test-e2e-k8s`, with STRICT=1.

### 2026-10-23

Full acceptance, STRICT=1 (logs in ~/.cache/fire-039, deleted at the end):
- `make test-e2e-cleanup`: 3 of 3 pass. k3d run a4edaz and compose run l6dc98 exit 143 and leave nothing.
- `make test-e2e`: 9 of 9 stacks pass in 22 min. The snapshot equals the baseline.
- `make test-e2e-k8s`: 13 of 14 pass in 30 min. The snapshot equals the baseline. `e2e_k8s_odoo_backup` fails.

Defect 1 (fixed): `firestream-e2e-odoo-backup-<id>` has 33 characters, and k3d allows 32. The odoo round-trip now uses the cluster target `odoo-bkp`. `reserve_name` now fails early with a clear message. A new unit test covers the limit. `cargo test -p firestream-e2e-core`: 75 pass.

Defect 2 (open, outside FIRE-039): on the rerun, the cluster comes up. Then the `odoodump` job fails with `gzip: command not found`. The odoo image has no gzip. The host snapshot still equals the baseline after the failure.

`nix flake check --no-build --all-systems` passes again.

Files: `makefile`, `CLAUDE.md`, `docs/supabase.md`, `firestream-e2e-core/src/{cleanup.rs,interrupt.rs,lib.rs,k8s/cluster.rs}`, `firestream-e2e-core/tests/leak_detection.rs`, `firestream-e2e-k8s/src/odoo_backup.rs`, `firestream-e2e-k8s/tests/e2e_k8s_interrupt.rs`, `firestream/tests/e2e_interrupt.rs`, `firestream/tests/e2e/harness.rs`.

Deviations:
- `cleanup.rs` (from FIRE-035) changed. The drain stops subprocesses first, and guard threads park during a drain.
- The compose preflight gives 20 s of grace to a port holder outside docker.
- New make target `test-e2e-cleanup`.
- Snapshots use the host docker CLI. The devshell CLI 29.6.1 lists 11 more untagged ids that the host CLI does not.

Status stays `active`. The full `make test-e2e-k8s` is not green, because of defect 2.

### 2026-10-24

Defect 2 is fixed in [[FIRE-059]]. `make test-e2e-k8s-odoo-backup` with STRICT=1 passes: 1 passed in 376 s, round-trip OK. The host snapshot has the same object names before and after the run.

### 2026-10-24

PM close on 2026-10-24. The last open item was the odoo-backup failure. [[FIRE-059]] fixed it: gzip was never in the image, the backup job did not load the image env, and the test wrote outside the volume. `make test-e2e-k8s-odoo-backup` now passes, with the host snapshot unchanged.

The PM verified:
- `nix flake check --no-build --all-systems` passes.
- The odoo checks pass: `firestream-odoo-backup-tools`, `firestream-odoo-backup` and `odoo-render-fidelity`.

The PM fixed two string interpolations of source paths that made the flake check fail after a garbage collection: `"${pkgs.path}/…"` in the Studio package and `"${src}/…"` in `mkNodePackage.nix`. Both now use path concatenation, and the nextjs drvPath is unchanged.

Open follow-up: one place that assigns all host ports without overlap. See the ticket after this one.
