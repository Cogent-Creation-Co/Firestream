---
id: FIRE-037
title: "E2E-C: Docker harness project names, early guard, egress helper, dangling images"
type: feature
status: done
priority: high
tags: [e2e, docker]
affects: []
depends-on: [FIRE-035]
---

# E2E-C: Docker harness project names, early guard, egress helper, dangling images

Epic: [[FIRE-034]]

## Scope

1. **Own project names.** The `-up` and `-down` apps (`nix/flake-modules/compose.nix`) read `FIRESTREAM_COMPOSE_PROJECT` and fall back to today's project name. The harness (`src/lib/rust/firestream/tests/e2e/harness.rs`) sets `firestream-e2e-<stack>-<id>`, so dev stacks are never swept. A port preflight fails with a clear message when a dev stack holds a port.
2. **Arm `StackGuard` before `up`.** Register it with the cleanup registry from [[FIRE-035]].
3. **Full teardown.** The guard, the pre-clean and the fallback use `down -v --remove-orphans`. The `-down` app's default stays the same: it does not remove volumes.
4. **Egress helper** (`probes.rs`):
   - start it with `--rm` and the label `com.docker.compose.project=<project>`;
   - stop it with `docker rm -f -v`;
   - register it with the cleanup registry.
5. **Dangling images.** The load step (`nix/flake-modules/docker-build.nix`) reads the image ID of `repo:tag` before `docker load`. If the old image is dangling after the load, the step removes it. Base images and their labels do not change.
6. **Leak check** after every stack.

## Acceptance

- `make test-e2e-supabase` passes and leaves nothing. That includes the egress helper and its volumes. No dangling `firestream-*` image is left after a reload.
- A running `nix run .#supabase-up` dev stack is untouched by an e2e run and by the sweep.
- The compose files for all stacks are byte-identical when `FIRESTREAM_COMPOSE_PROJECT` is unset (same-tree comparison).

## Work log

### 2026-10-23

Work starts. Scope: compose project override, early guard, egress helper, dangling images, leak check.

### 2026-10-23

Review found the earlier work mostly correct. All code compiles. `cargo test -p firestream-e2e-core` passes 71 tests plus 3 doc tests. `cargo build --workspace` passes. `cargo test -p firestream --test e2e --no-run` builds clean.

Byte-identity proven for all 31 compose stacks. The project-name lines are the only functional diff; the compose YAML hash is identical with and without the change, checked in the same tree.

The dangling-image removal logic was proven with a real `docker load` of two distinct single-tag images: the displaced image is removed, the new image stays, and the pre-existing dangling images on the host are untouched.

Blocker: `make test-e2e-supabase` cannot complete. The Envoy image needs a fresh Nix build (its store path was garbage-collected), and the Envoy Bazel deps fetch (`envoy-1.36.2-deps.tar.gz`) fails with a fixed-output hash mismatch. This is a pre-existing reproducibility defect in the Envoy container build, not a FIRE-037 regression. Per instruction, the Envoy depsHash and build are left untouched. Verification continues with `make test-e2e-postgresql` and one other stack that builds, instead of supabase.

### 2026-10-23

Code check passes. The egress helper uses `--rm`, the compose project label and `docker rm -f -v`. `FIRESTREAM_E2E_KEEP=1` skips teardown and the leak check, and prints `make e2e-sweep RUN=<id>`.

Fix: a failed helper `docker run` now runs the registered cleanup. Before, it only deregistered it.

`make test-e2e-postgresql` passes (run `e0ws06`, rc 0). The containers, networks and volumes are identical before and after. The reload removed the displaced `firestream-postgresql:17` image `750305c69e8a`.

Finding: on this containerd image store, `docker images -f dangling=true` also lists some tagged images. The snapshot now compares only untagged image IDs.

### 2026-10-23

`make test-e2e-supabase` run 1 (`xygbrg`) passes. The egress probe passes. Containers, networks, volumes and untagged images are identical before and after. The reload removed 11 displaced images.

Runs 2 (`xtsbgj`) and 4 (`m7nsed`) fail in `supabase-up`: auth and realtime exit 1. The host is identical before and after both runs, so the early guard works.

Run 3 (`ikn4oy`) broke: a `nixos-rebuild` switch stopped dockerd mid-run. After the restart the host has no leftovers. Only the default `bridge` network has a new ID.

Defect, outside FIRE-037: the supabase-postgres healthcheck passes before the migrations end. `readinessCmd` is `pg_isready -h 127.0.0.1`, and the init server answers it. Auth and realtime then start without the `_supabase` database or their schema.

Fix: the harness now dumps compose diagnostics when `up` fails. Before, the teardown removed the logs first.

Finding: every run gives new image IDs for unchanged sources, so every run reloads and removes the displaced images.

### 2026-10-23

`make test-e2e-supabase` run 5 (`ib9tlx`) passes. It loads no new image. The host is identical before and after.

Dev-stack proof: `nix run .#supabase-up` starts 11 containers in project `firestream-supabase`. The e2e supabase run then fails in the port preflight. It names all four held ports and the holders. It builds nothing and starts nothing.

`firestream-e2e-sweep --dry-run` and a real sweep both find no harness-owned resources. The dev containers and volumes are identical after the sweep. `nix run .#supabase-down -- -v` then restores the host snapshot.

Real reload proof: the reloads removed the displaced images `750305c69e8a`, `90597c59e793`, `d6f21240d9e2` and `e177b71c850e`. The tagged images stay.

### 2026-10-23

FIRE-037 is complete.

**Files.**
- `nix/flake-modules/compose.nix`: the `-up` and `-down` apps read `FIRESTREAM_COMPOSE_PROJECT`.
- `nix/flake-modules/docker-build.nix`: the load step removes the displaced image.
- `src/lib/rust/firestream/tests/e2e/harness.rs`: lock, signals, run project names, preflight, early guard, full teardown, leak check. It dumps diagnostics when `up` fails.
- `src/lib/rust/firestream/tests/e2e/probes.rs`: egress helper with `--rm`, the label, `rm -f -v` and cleanup registration. A failed start runs the cleanup.

**Verification.**
- `make test-e2e-postgresql` passes once.
- `make test-e2e-supabase` passes twice (`xygbrg`, `ib9tlx`).
- Every snapshot after a run is identical to the snapshot before it. The host had no `k3d-*` objects.
- The dev stack survives the preflight and the sweep.
- `cargo test -p firestream-e2e-core` passes 73 tests and 3 doc tests.

**Deviations.**
- The snapshot compares untagged image IDs. On the containerd store, `dangling=true` also lists tagged images.
- `make e2e-sweep` does not exist yet. [[FIRE-038]] adds it. The verification used `cargo run --bin firestream-e2e-sweep`.
- Two supabase runs failed from the db readiness defect. A third broke when `nixos-rebuild` stopped dockerd. None of them leaked.

**Open defects.**
- The supabase-postgres healthcheck reports ready before the migrations end. Make a ticket for it.
- Image IDs change between builds of unchanged sources.
