---
id: FIRE-030
title: "Supabase e2e (docker and k8s) and CI profile phases"
type: feature
status: done
priority: medium
tags: [e2e, phase-3, supabase]
affects: []
depends-on: [FIRE-028, FIRE-029]
---

# Supabase e2e (docker and k8s) and CI profile phases

Epic: [[FIRE-009]]

## Goal

Test the Supabase stack end to end on both targets, and add the tests to the
advisory `--mode e2e` chain.

## Scope

- Docker e2e: add `supabase` to `CANONICAL` in
  `src/lib/rust/firestream/tests/e2e/stacks.rs`, a `stack_test!` line in
  `tests/e2e.rs`, and probes in `tests/e2e/probes.rs`.
- k8s e2e: add `stack_test_k8s!(e2e_k8s_supabase, "supabase")` in
  `src/lib/rust/firestream-e2e-k8s/tests/e2e_k8s.rs`. Add a `for_chart` arm
  in `src/lib/rust/firestream-e2e-core/src/k8s/probes.rs` and update the
  canonical list test.
- Probes: Auth `/health`, PostgREST with the anon key, a Storage upload and
  download, a Realtime websocket change event, a query through Supavisor,
  and Studio.
- CI profile: add `supabase` to `e2eDockerStacks` and `e2eK8sCharts` in
  `bin/nix/firestream/ci/profile.nix`.

## Acceptance

- `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass.
- `make ci-e2e-dry-run` shows the two new advisory phases in the chain.
- The 12 tests in `src/util/firestream-ci/tests/e2e_phase_chain.rs` pass.

## Phase 2 facts

- **Probe hints:**
  - Realtime: the tenant health route `/api/tenants/realtime-dev/health` needs the anon JWT.
  - Gateway: `/realtime/v1/api/tenants` returns 403 (upstream behaviour), and `?apikey=sb_publishable_…` on `/rest/v1/` returns 400 (upstream behaviour).
  - Supavisor: `/api/health` returns 204.
  - pg_graphql is off by default (upstream migration `20260421000000`).
- **Studio traffic:** capture its outbound traffic in the e2e run. Prove that no PostHog or ConfigCat request leaves the stack. See [[FIRE-025]].
- **CI selection:** the `verify` phase builds a fixed set of flake checks (`bin/nix/firestream/ci/profile.nix`, around line 287). Phases 1 and 2 added new checks. Decide which of them `verify` must run, and register them there.

## Work log

### 2026-10-23

e2e and CI phases done. `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass.

Files:
- `src/lib/rust/firestream-e2e-core/src/supabase.rs` (new): the shared probe chain (`CHAIN`, `probe`, `probes`, `SupabaseStack`). Probes: Auth `/auth/v1/health` with the anon key; PostgREST reads a table with the anon key; Storage bucket, upload, download and compare; a Realtime `postgres_changes` INSERT over a websocket (a Node script in the Studio container joins the channel and inserts through PostgREST); `select 40 + 2` through Supavisor 6543 as `postgres.firestream`; Studio 401 without and 200 with the dashboard login. Keys come from `printenv` in the running gateway and db.
- `firestream-e2e-core/src/probe.rs`: `impl Probe for Box<P>`. `src/lib.rs`: the module.
- `firestream-e2e-core/src/k8s/probes.rs`: the `supabase` arm. A session opens the gateway port-forward on the first run and resets after a failure; pods are found by component label per call. The canonical list test now has `nginx` and `supabase`.
- `firestream-e2e-k8s/src/harness.rs`: `stack_prerequisites`. When the bundle has a stack with the chart's name, the harness deploys and waits for the members before the chart. No other chart has such a stack, so their runs do not change.
- `firestream-e2e-k8s/tests/e2e_k8s.rs`: `stack_test_k8s!(e2e_k8s_supabase, "supabase")`.
- `src/lib/rust/firestream/tests/`: `supabase` in `CANONICAL` (9), `stack_test!(e2e_supabase, ...)`, the supabase chain in `probes.rs`, and `StudioEgressCapture` with two unit tests.
- `bin/nix/firestream/ci/profile.nix`: `supabase` in `e2eDockerStacks` (last) and `e2eK8sCharts` (after nginx); the header no longer claims "13 granular checks".
- `nix/flake-modules/supabase.nix`: `apps.supabase-{postgrest,supavisor,postgres-meta,imgproxy}-image`. The k3d preload derives `.#<repository minus firestream->-image`, and these four repositories do not match their registry keys.

Studio traffic capture: in the docker e2e, a helper container joins Studio's network namespace and runs the flake's nixpkgs tcpdump (host /nix/store mounted read-only). The probe loads eight Studio pages and API routes, then fails on a DNS query that names PostHog or ConfigCat or on an outbound packet that leaves the compose network. A capture that misses Studio's postgres-meta traffic fails as not capturing. Result: 121 packets, no DNS lookup, no outbound packet. The first run flagged received IPv6 mDNS multicast; only `Out` packets now count, and link-local and multicast stay local.

CI selection: `verify` already derives its list from the flake checks (minus `firestream-tests`), so every Phase 1-3 check is gated: `firestream-beam-container`, `firestream-pnpm-lock-json`, `firestream-postgresql-extensions-seam`, `firestream-postgresql-supabase-extensions`, `firestream-supabase-dev-secrets`, `firestream-supabase-postgres-migrations`, `supabase-render-fidelity`. I keep all of them; none needs an exclusion.

Verification:
- `make test-e2e-supabase`: 9 passed (all 8 probes and the egress capture), 123 s.
- `make test-e2e-k8s-supabase`: 14 passed; SeaweedFS Ready, then all 6 shared probes, 390 s.
- `make test-e2e-k8s-seaweedfs`: 13 passed.
- `make ci-e2e-dry-run`: 22 advisory phases; `e2e-docker-supabase` and `e2e-k8s-supabase` are in the chain with `depends_on=verify,<previous>`.
- `make ci-dry-run`: exit 0; verify lists the new checks; build lists `supabase-chart`.
- `src/util` `cargo test -p firestream-ci`: `e2e_phase_chain` 12 passed.
- `cargo test -p firestream-e2e-core -p firestream-e2e-k8s`: 37 and 19 passed. `cargo test -p firestream --test e2e`: 2 passed, 9 ignored.
- `nix flake check --no-build --all-systems`: exit 0.

Deviations: the egress capture runs in the docker e2e only (the k8s run uses the same image). It covers Studio's server side; requests that a browser makes from Studio's client bundle are outside it.

Open items:
- The k8s harness teardown leaves an empty k3d Docker network per cluster (also for SeaweedFS). I removed the ones my runs left.
- No test drives Studio's client bundle in a browser.
