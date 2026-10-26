---
id: FIRE-009
title: "Self-hosted Supabase stack packaged with Nix"
type: plan
status: done
priority: high
tags: [epic, supabase]
affects: []
depends-on: [FIRE-010, FIRE-011, FIRE-012, FIRE-013, FIRE-014, FIRE-015, FIRE-016, FIRE-017, FIRE-018, FIRE-019, FIRE-020, FIRE-021, FIRE-022, FIRE-023, FIRE-024, FIRE-025, FIRE-026, FIRE-027, FIRE-028, FIRE-029, FIRE-030, FIRE-031, FIRE-032, FIRE-033, FIRE-040, FIRE-041, FIRE-057, FIRE-058]
---

# Self-hosted Supabase stack packaged with Nix

This is the epic. Its `depends-on` lists every child ticket. Close it only
when all children are done.

## Goal

Ship a complete self-hosted Supabase in Firestream. Package each service
the same way as Airflow and the other apps: a Nix-built container, a typed
options module, a compose stack and a Helm chart.

## Decisions

- Build every image with Nix from day one. Use no upstream images.
- Build the database as a Supabase variant of the firestream `postgresql`
  image. Do not use the `supabase/postgres` flake.
- Use Envoy as the API gateway. Envoy replaced Kong as the upstream default
  in August 2026.
- Use SeaweedFS as the S3 backend for Supabase Storage.
- Give Supabase its own stack, `firestreamStacks.supabase`. Do not add it to
  `dev`, because it has its own Postgres.
- Support both targets: a compose stack and a Helm chart, with e2e for both.

- Build the Node dependencies with pnpm2nix: one Nix derivation for each locked package. Build native modules from source; do not ship prebuilt `.node` files. This applies to Storage, postgres-meta and Studio. A project with only a `package-lock.json` gets a `pnpm-lock.yaml` through `pnpm import`. Decided on 2026-10-22 after the spike [[FIRE-013]] found prebuilt binaries in `@datadog/pprof`.

- No import-from-derivation (IFD), the same rule as the rest of the repo. pnpm2nix converts the YAML lockfile to JSON at evaluation time, so the project uses a vendored copy of the patched pnpm2nix that reads a JSON lockfile committed to the repo. A check proves that each JSON lockfile matches its YAML source. Cargo locks use `fetchCargoVendor`, a fixed-output fetch. Decided on 2026-10-23. See [[FIRE-032]].
- A prebuilt WASM artifact from npm (Studio's `libpg-query`) is allowed when the lockfile pins it by hash. WASM is platform-independent bytecode. Native `.node` files stay forbidden. Decided on 2026-10-23.

## Scope

| Service | Upstream pin (docker v0.8.3) | Build route |
|---|---|---|
| Postgres | `supabase/postgres:17.6.1.136` | firestream postgresql + extensions |
| Auth (GoTrue) | `supabase/gotrue:v2.197.0` | Go, `buildGoModule` |
| PostgREST | `postgrest/postgrest:v14.18` | Haskell, nixpkgs |
| Envoy | `envoyproxy/envoy:v1.39.1` | nixpkgs |
| imgproxy | `darthsim/imgproxy:v3.31.4` | Go, nixpkgs |
| Storage API | `supabase/storage-api:v1.81.0` | Node |
| postgres-meta | `supabase/postgres-meta:v0.100.0` | Node |
| Studio | `supabase/studio:2026.10.12-sha-3699278` | Node (Next.js, pnpm monorepo) |
| Realtime | `supabase/realtime:v2.143.3` | Elixir |
| Supavisor | `supabase/supavisor:2.9.13` | Elixir |

## Out of scope

- Edge Functions (`supabase/edge-runtime`): Rust plus Deno V8.
- Logflare and Vector analytics.
- Hosted-only features: branching and the Studio platform API.

## Phases

| Phase | Content |
|---|---|
| 0: Spikes | Elixir build, Studio build, migrations on our Postgres, Storage on SeaweedFS |
| 1: Foundation | supabase_vault, Postgres variant, migrations, BEAM factory, secrets |
| 2: Service images | One image for each service |
| 3: Integration | Compose stack, Helm chart, e2e and CI, documentation |

The spikes come first. A "no-go" result from a spike changes the plan before
we start the dependent work.

## Sources

- https://github.com/supabase/supabase/tree/master/docker
- https://github.com/supabase/supabase/blob/master/docker/CHANGELOG.md
- https://github.com/supabase/postgres
- https://github.com/supabase-community/supabase-kubernetes
- https://supabase.com/changelog/48048-self-hosted-supabase-envoy-becomes-the-default-api-gateway-b
- https://github.com/seaweedfs/seaweedfs/wiki/Amazon-S3-API

## Work log

### 2026-10-23

Phase 3 done: compose stack (FIRE-028), Helm chart and `firestreamStacks.supabase` (FIRE-029), docker and k8s e2e with CI phases (FIRE-030), documentation (FIRE-031). Every child ticket FIRE-010 to FIRE-032 is done. `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass.

### 2026-10-23

PM close on 2026-10-23. Every child ticket is done: FIRE-010 to FIRE-033, FIRE-040, FIRE-041 (pinned source builds), FIRE-057 (Realtime JWKS) and FIRE-058 (Postgres readiness).

The final checks, run by the PM:
- `nix flake check --no-build --all-systems` passes.
- All 12 `*-render-fidelity` checks pass.
- `supabase-no-prebuilt-binaries` passes: 12 app outputs, every binary on the allowlist.
- `firestream-supabase-dev-secrets` passes.
- [[FIRE-057]] ran both Supabase e2e suites with the `sb_publishable_*` Realtime probe: Docker 9 passed, k3d 14 passed, and the host snapshots were identical.

Open follow-ups are outside this epic: the e2e clean-teardown work in [[FIRE-034]] / [[FIRE-039]].
