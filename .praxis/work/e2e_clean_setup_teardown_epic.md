---
id: FIRE-034
title: "Clean e2e setup and teardown"
type: plan
status: done
priority: high
tags: [e2e, epic]
affects: []
depends-on: [FIRE-035, FIRE-036, FIRE-037, FIRE-038, FIRE-039, FIRE-059]
---

# Clean e2e setup and teardown

This is the epic. Its `depends-on` lists every child ticket.

## Goal

Every e2e run starts from a clean host and leaves a clean host. A leak fails the test. Interrupted and crashed runs get cleaned. Cleanup never touches resources that the harness does not own.

## Root causes

| # | Harness | Gap |
|---|---|---|
| 1 | k3d | `ClusterGuard` deletes the cluster before the registry, so the `k3d-<name>` network stays (`firestream-e2e-core/src/k8s/cluster.rs:243-338`; same order in `k8s-manager/src/providers/k3d/operations.rs:553-588`). |
| 2 | k3d | The guard is armed only after create succeeds, so a partial create leaks (`cluster.rs:160-185`). |
| 3 | k3d | The `docker rm -f` fallback never removes the network or the `k3d-<name>-images` volume. |
| 4 | k3d | k3d updates the default kubeconfig and switches context, and the library runs `use-context` (`operations.rs:494-546`). |
| 5 | Docker | `StackGuard` is armed after `up` (`firestream/tests/e2e/harness.rs:484-491`). |
| 6 | Docker | The egress helper has no `--rm` and no label, and it is removed without `-v` (`probes.rs:449-504`). |
| 7 | Docker | e2e uses the same compose project names as dev stacks. |
| 8 | Docker | `down -v` has no `--remove-orphans`. |
| 9 | Docker | A reload leaves a dangling `<none>` image (`docker-build.nix:124-146`). |
| 10 | both | No SIGINT/SIGTERM handling. |
| 11 | both | No pre-run sweep, no cross-process lock, no leak assertion. |

## Decisions (user, 2026-10-23)

- Keep the tagged images as a cache. Remove only the dangling firestream images that a reload leaves.
- A leak fails the test (exit 2 in the advisory CI phases).
- Remove all old leftovers on this host, including the dangling anonymous volumes. List them first, and never use `docker volume prune`.

## Parts

A shared core; B k3d harness; C Docker harness; D make, CI, tests and docs; E one-time host cleanup. A runs first. B and C run in parallel after A. D runs after B and C. E runs after A.

## Work log

### 2026-10-24

PM close on 2026-10-24. Every child ticket is done: FIRE-035 to FIRE-039 and FIRE-059.

Result:
- Each e2e run starts with a sweep and ends with a leak check, under a lock that covers the host.
- SIGINT and SIGTERM run the registered teardowns.
- The harness never touches dev stacks, and it does not change the global kubeconfig.
- Full `make test-e2e` passed (9 of 9 stacks), and `make test-e2e-k8s` passed (14 of 14 after [[FIRE-059]]), with host snapshots equal to the baseline.
- The interrupt tests and the leak-detection test pass.
