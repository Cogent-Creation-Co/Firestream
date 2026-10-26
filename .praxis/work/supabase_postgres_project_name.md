---
id: FIRE-061
title: "supabase-postgres compose stack has its own project name"
type: feature
status: backlog
priority: medium
tags: [compose, supabase]
affects: []
depends-on: []
---

# supabase-postgres compose stack has its own project name

Found in [[FIRE-060]].

## Problem

`supabase-postgres-compose` uses the compose project name `firestream-postgresql`, the same as the postgresql stack. The variant inherits the base options' project name. So `nix run .#supabase-postgres-up` and `nix run .#postgresql-up` share one compose project: one stack's `down` removes the other's containers, and `--remove-orphans` treats them as orphans. Their host ports already differ through the registry ([[FIRE-060]]).

## Scope

- Give the Supabase variant its own project name, for example `firestream-supabase-postgres`, in `src/containers/firestream/postgresql/supabase.nix`.
- Add a rule to the port registry check (`checks.firestream-port-registry`) or to a new check: no two compose stacks share a project name, except the variants that `stackOf` maps to one stack on purpose.

## Acceptance

- The check fails when two different stacks share a project name, and it passes on the fixed tree.
- `nix run .#postgresql-up` and `nix run .#supabase-postgres-up` run together, and each `-down` removes only its own containers.
- `nix flake check --no-build --all-systems` passes.
