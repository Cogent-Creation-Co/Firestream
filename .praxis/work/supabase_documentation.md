---
id: FIRE-031
title: "Supabase documentation"
type: feature
status: done
priority: medium
tags: [docs, phase-3, supabase]
affects: []
depends-on: [FIRE-029]
---

# Supabase documentation

Epic: [[FIRE-009]]

## Goal

Document the Supabase stack for users and for maintainers.

## Scope

- `docs/firestream-supported-app.md` §2: add the Supabase images and chart.
- `CLAUDE.md`: update the Helm Chart Contract (charts list, stacks), the
  e2e harness sections and the BEAM factory.
- A user page: how to start the stack, the development secrets, how to
  supply production secrets, and the gateway URLs.
- A maintainer page: how to sync the vendored migrations, the Envoy config
  and the pinned versions with a new upstream docker release.

## Acceptance

- The docs name every Supabase image, the chart and the `supabase` stack.
- The sync steps are complete enough to update to a new upstream release.

## Phase 2 facts

- CLAUDE.md says "11 container images". The count is now out of date.
- Document the version gaps against upstream v0.8.3: PostgREST 14.1 (upstream 14.18), Envoy 1.36.2 (upstream 1.39.1). Realtime has no `pgdelta`.
- Document the build costs:
  - Envoy compiles from source in about 50 minutes on 32 cores. Its Bazel dependency hash is overridden, and that fetch is not reproducible over time.
  - Studio's closure is about 810 MiB.
  - `firestream.packages.goToolchain` (Go 1.27.3) exists because Auth and imgproxy need a newer Go than the pin.
- Document the new library pieces: `firestream.supabase`, `mkMixRelease`, `mkBeamContainerModule`, the vendored pnpm2nix ([[FIRE-032]]) and `supabase-keygen`.

## Work log

### 2026-10-23

Documentation done.

Files:
- `docs/supabase.md` (new, user page): the ten services and their gateway paths, `nix run .#supabase-up` / `-down`, the host ports, the development secrets and their risk, `firestream helm deploy-stack supabase`, production secrets with `supabase-keygen --output k8s-secret` and `secret.supabase.secretRef`, the S3 and SMTP secrets, the public URLs, and the `migration` value.
- `docs/supabase-maintainer.md` (new, maintainer page): a table of every pin and its file, the eight-step sync to a new upstream release (sources, db files, Envoy config, services, pnpm locks, chart tags, chart templates, verification), the version gaps (PostgREST 14.1 against 14.18, Envoy 1.36.2 against 1.39.1, no `pgdelta`, pg_graphql off), the build costs (Envoy about 50 min on 32 cores with a `depsHash` that is not reproducible over time, Studio about 810 MiB, Go 1.27.3), and the library pieces (`firestream.supabase`, `supabase-keygen`, `mkMixRelease`, `mkBeamContainerModule`, the vendored pnpm2nix, `goToolchain`).
- `docs/firestream-supported-app.md` §2: Supabase as the multi-image app (ten images, the forked chart, the compose stack on the gateway image).
- `CLAUDE.md`: Elixir (BEAM) in the language list; `supabase` in the chart list; three stacks; a Supabase paragraph in the Helm Chart Contract; the k8s harness lists and `stack_prerequisites`; the e2e chain counts (22 phases, docker ×9, k8s ×13, 13 k3d clusters, 9 compose stacks); the build counts (21 images, 11 charts).

Verification: I checked each file path, option name and command in the pages against the tree. The claims about routes come from the vendored Envoy `lds.template.yaml`.

Open items: the `migration` value is rendered and mounted, but no test runs a user SQL file on a cluster.
