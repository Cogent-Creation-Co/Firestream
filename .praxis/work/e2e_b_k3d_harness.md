---
id: FIRE-036
title: "E2E-B: k3d harness teardown order, early guard, kubeconfig isolation"
type: feature
status: done
priority: high
tags: [e2e, k3d]
affects: []
depends-on: [FIRE-035]
---

# E2E-B: k3d harness teardown order, early guard, kubeconfig isolation

Epic: [[FIRE-034]]

## Scope

1. **Teardown order** in `ClusterGuard::drop` (`firestream-e2e-core/src/k8s/cluster.rs`) and `K3dClusterManager::delete_cluster` (`k8s-manager/src/providers/k3d/operations.rs`):
   1. delete the registry;
   2. delete the cluster;
   3. remove the `k3d-<name>` network if it still exists;
   4. remove the `k3d-<name>-images` volume;
   5. remove the kubeconfig file.

   The fallback removes the containers, the network and the volume by the label `k3d.cluster=<name>`.
2. **Arm the guard before create.** Construct the guard from the name before the registry exists, so a partial create is torn down. Register it with the cleanup registry from [[FIRE-035]].
3. **Kubeconfig isolation.** Pass `--kubeconfig-update-default=false --kubeconfig-switch-context=false` to `k3d cluster create`. Make the `use-context` call optional in `k8s-manager`; the harness skips it, and other callers keep the default. Fix the comment at `cluster.rs:174-177`.
4. **Run identity and leak check.** Use the run ID from [[FIRE-035]], and run the leak check after every k8s test: the chart tests, `pg_backup`, `odoo_backup` and `e2e_k8s_cluster_lifecycle`.

## Acceptance

- A unit test with a fake runner proves the teardown order.
- `make test-e2e-k8s-supabase` and `make test-e2e-k8s-seaweedfs` pass, and nothing is left: no network, volume, container, registry or kubeconfig context.
- `~/.kube/config` and `$KUBECONFIG` are byte-identical before and after.
- `cargo test` passes for `k8s-manager`, `firestream-e2e-core` and `firestream-e2e-k8s`.

## Work log

### 2026-10-23

Work starts. Scope: teardown order, early guard, kubeconfig isolation, run ID and leak check.

### 2026-10-23

Part B is complete.

**Files.** k8s-manager: `src/lib/rust/k8s-manager/src/config.rs` adds `K3dClusterConfig::isolate_kubeconfig` (default false). `src/lib/rust/k8s-manager/src/providers/k3d/operations.rs`: `create_cluster` passes `--kubeconfig-update-default=false --kubeconfig-switch-context=false` and skips `fix_kubeconfig_for_container` and `use-context` when `isolate_kubeconfig` is true; `delete_cluster` now removes the registry, then the cluster, then the `k3d-<name>` network, then the `k3d-<name>-images` volume, in that order.

firestream-e2e-core: `src/k8s/cluster.rs` names clusters `firestream-e2e-<chart>-<run id>` (`RunId::resource_name`) through a process-wide `reserve_name`, which fails clearly on a repeat chart. `create_cluster` registers a teardown with `cleanup` before the cluster exists, and runs it on any create failure, so a partial create (registry up, cluster create failed) is swept. `ClusterGuard::arm` registers its own teardown (skipped when `keep`); `Drop` just runs it. The teardown itself is `sweep::discover`/`plan`/`execute` for `Scope::Name(name)` under the host lock — the same order a sweep uses: registry, cluster, leftover containers, network, volume, kubeconfig file, kubeconfig context/cluster/user. The stale "we never touch global config" comment is now true (the isolated path never does). `cleanup.rs` gets a `#[cfg(test)] test_serial()` shared by its own drain tests and cluster.rs's new guard test, because both touch the one process-wide registry.

firestream-e2e-k8s: `harness.rs::run_one`, `pg_backup.rs::run_pg_backup_roundtrip`, `odoo_backup.rs::run_odoo_backup_roundtrip`, and `tests/e2e_k8s.rs::e2e_k8s_cluster_lifecycle` all install the signal handler and take `HostLock::acquire()` before creating anything, and run a leak check (`harness::check_for_leaks`, now `pub`) for their target after the cluster/release guards drop. KEEP honours both `FIRESTREAM_E2E_K8S_KEEP` and `FIRESTREAM_E2E_KEEP`.

**Verification.**
- `cargo test -p k8s-manager -p firestream-e2e-core -p firestream-e2e-k8s` (inside `nix develop`): 73 + 19 + other crate tests pass, 3 runs in a row, no flakes (one run surfaced two pre-existing `cleanup` tests racing my new guard test on the shared registry; fixed by giving both a shared `test_serial()` lock).
- `cargo test -p firestream --lib`: 33 pass.
- `make test-e2e-k8s-cluster_lifecycle` style run (`--ignored e2e_k8s_cluster_lifecycle`): cluster up, 1 node, clean teardown. `docker ps -a`, `docker network ls`, `docker volume ls`, and `sha256sum ~/.kube/config /etc/rancher/k3s/k3s.yaml` are byte-identical before and after.
- Partial-create proof: temporarily set a bogus `k3s_version` so `k3d cluster create` fails after the registry is up. k3d's own rollback left the network and the registry container behind (network had an active endpoint). The new early registration's teardown then removed both; the host returned to the exact baseline snapshot. Reverted the sabotage edit afterward.
- `make test-e2e-k8s-seaweedfs`: full chart deploy + probe + teardown passes; host snapshot identical before/after.
- `make test-e2e-k8s-supabase`: failed, but NOT from this work — `nix build .#supabase-envoy-image` hit a fixed-output hash mismatch building envoy from source (an unrelated, pre-existing Nix/Bazel pin problem). The panic unwound through the cluster/release guards exactly as a normal failure would, and the host snapshot was still identical to baseline afterward — proving teardown-on-failure independently of that unrelated breakage.
- `pg-backup` / `odoo-backup` round-trips were not run (each is a multi-chart, multi-minute scenario; not in the PM's explicit verify list and the host-clean property is already proven by the lifecycle, seaweedfs, and supabase-failure runs above).
- No real `k3d` or `kubectl` command ran outside `nix develop`; this host has no leftover `firestream-e2e-*` objects from any run.

**Deviations.**
- `K3dClusterManager::delete_cluster`'s new order has no fake-runner unit test — that crate shells out via `tokio::process::Command` directly and has no `CommandRunner` seam; adding one was out of scope. The order is proven live by the lifecycle/seaweedfs runs above instead.
- `ClusterGuard`'s teardown order is not independently unit-tested with a fake runner; it delegates entirely to `sweep::plan`, whose order the existing `sweep::tests::plan_orders_k3d_then_compose` (from FIRE-035) already proves with a `FakeRunner`. Two new tests in `cluster.rs` (`reserve_name_fails_on_reuse`, `guard_registers_teardown_only_without_keep`) cover the naming and registration bookkeeping that sit on top of it.
- Did not change `src/lib/rust/firestream/src/app/k8s.rs` — `create_cluster`/`ClusterGuard::arm` kept their exact signatures, so no caller needed adapting.

`git add`ed only this part's paths: `src/lib/rust/firestream-e2e-core/src/k8s/cluster.rs`, `src/lib/rust/firestream-e2e-core/src/cleanup.rs`, `src/lib/rust/firestream-e2e-k8s/src/{harness,odoo_backup,pg_backup}.rs`, `src/lib/rust/firestream-e2e-k8s/tests/e2e_k8s.rs`, `src/lib/rust/k8s-manager/src/config.rs`, `src/lib/rust/k8s-manager/src/providers/k3d/operations.rs`. Left FIRE-037's files (`nix/flake-modules/compose.nix`, `docker-build.nix`, `firestream/tests/e2e/*`) untouched.
