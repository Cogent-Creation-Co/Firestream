---
id: FIRE-035
title: "E2E-A: shared run identity, lock, signal cleanup, sweeper and leak check"
type: feature
status: done
priority: high
tags: [e2e]
affects: []
depends-on: []
---

# E2E-A: shared run identity, lock, signal cleanup, sweeper and leak check

Epic: [[FIRE-034]]

## Scope (`src/lib/rust/firestream-e2e-core/`)

1. **Run identity:** one run ID for each harness process. Resource names are `firestream-e2e-<target>-<id>`.
2. **Host-wide lock:** an exclusive `flock` on `${XDG_RUNTIME_DIR:-/tmp}/firestream-e2e.lock`, next to the in-process `harness_lock()` and `e2e_lock()`. Only the holder runs tests or sweeps.
3. **Cleanup registry and signals:**
   - A process-wide registry of active teardowns. Guards register when armed and deregister on drop.
   - A SIGINT/SIGTERM handler (`ctrlc` with the `termination` feature) runs every registered teardown, then exits with 130 or 143.
   - The existing guard budgets (`guard.rs`) stay.
4. **Sweeper binary `firestream-e2e-sweep`.** It runs under the lock and removes only harness-owned resources:
   - k3d clusters named `firestream-e2e-*`;
   - containers, registries, networks and volumes named `k3d-firestream-e2e-*` and labelled `app=k3d`;
   - compose projects labelled `com.docker.compose.project=firestream-e2e-*`. For these it reads `.Mounts` first, then removes the containers, the anonymous volumes, the named volumes and the network;
   - `$TMPDIR/firestream-e2e-*.kubeconfig` files;
   - kubeconfig contexts named `k3d-firestream-e2e-*`.

   Modes: `--dry-run` and `--run <id>`.
5. **Leak-check API:** after teardown, find every object that carries the run's names or labels. Return a failure that lists the objects and the sweep command. `FIRESTREAM_E2E_KEEP=1` skips it and prints `make e2e-sweep RUN=<id>`.

## Acceptance

- Unit tests with a fake command runner cover the name and label filters. The sweeper never matches a dev project (`firestream-<stack>` without `e2e-`) or a non-k3d object.
- `firestream-e2e-sweep --dry-run` runs on this host and lists the two known `k3d-firestream-e2e-*` networks.
- `cargo test -p firestream-e2e-core` passes.

## Work log

### 2026-10-23

Part A is complete in `src/lib/rust/firestream-e2e-core/`.

**Files.** New: `run_id.rs`, `lock.rs`, `cleanup.rs`, `runner.rs`, `sweep.rs`, `leak.rs`, `test_child.rs`, `bin/firestream-e2e-sweep.rs`. Changed: `lib.rs`, `guard.rs` (adds `TEARDOWN_BUDGET`), `Cargo.toml`, root `Cargo.toml`, `Cargo.lock`.

**Verification.**
- `cargo test -p firestream-e2e-core` passes 71 unit tests and 3 doc tests. Three runs gave the same result.
- Child-process tests prove lock exclusivity, SIGTERM exit 143, and SIGINT exit 130.
- A child-process test proves that a signal during a guard teardown waits for it and runs it once.
- Clippy shows no new warnings. The three remaining warnings are in older code.
- `nix develop --command cargo build --workspace` passes.
- The sweeper dry run lists only the two stale `k3d-firestream-e2e-*` networks.

**Deviations.**
- The `libc` workspace dependency records the signal number, because ctrlc does not report it. It adds no new crate to the lock.
- The sweeper also removes the kubeconfig cluster and user entries for `k3d-firestream-e2e-*`.
- A mutex now serialises two env-var tests in `k8s/images.rs`. They raced and failed at random.
- k3d is not on PATH outside the devshell. The sweeper then removes k3d objects with docker only.
