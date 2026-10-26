---
id: FIRE-033
title: "Supabase follow-ups: image key names and the chart key override"
type: feature
status: done
priority: medium
tags: [phase-3, supabase]
affects: []
depends-on: [FIRE-028, FIRE-029, FIRE-030, FIRE-031]
---

# Supabase follow-ups: image key names and the chart key override

Epic: [[FIRE-009]]

The user made two decisions in the Phase 3 review (2026-10-23).

## 1. Flake keys equal image repository names

Four images have flake keys that differ from their repository names
(`firestream-supabase-*`). So the k3d preload needs four alias apps in
`nix/flake-modules/supabase.nix`.

- Rename the flake keys: `postgrest` to `supabase-postgrest`, `supavisor` to
  `supabase-supavisor`, `postgres-meta` to `supabase-postgres-meta`, and
  `imgproxy` to `supabase-imgproxy`. This covers `packages.*`,
  `firestreamContainers.*`, `firestreamImages.*`, the chart `containerRefs`
  sources, `compose.nix` `@key` references, `containerPackages` in
  `profile.nix`, the docs, and any Rust or e2e reference.
- Delete the four alias apps.
- Rename the flake-module files to match, if that is the repo pattern.

## Acceptance

- `nix flake check --no-build --all-systems` passes.
- No alias app remains, and `make test-e2e-k8s-supabase` passes, preload
  included.
- A render without `secretRef` uses the dev keys and prints the NOTES
  warning. A render with `secretRef` contains no dev key value and no warning.
- `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass.
- `docs/supabase.md`, `docs/supabase-maintainer.md` and `CLAUDE.md` show the
  new names and the override.

## 2. Dev keys by default, with an override

The user decided (2026-10-23): keep the development key set in the chart as the default, and let a deployment override it.

- Keep the dev keys in `values.yaml`, and keep the check that they equal `firestreamLib.supabase.devSecrets`.
- `secret.supabase.secretRef=<name>` replaces the whole dev set with an existing Secret. Verify the override: a render with `secretRef` must contain no dev key value.
- Add a NOTES.txt warning that prints when the dev keys are in use, and does not print when `secretRef` is set.
- Document the production path: `nix run .#supabase-keygen -- --output k8s-secret --name <name>`, then `secret.supabase.secretRef=<name>`.

## Work log

### 2026-10-23

Part 1 done. Renamed the flake keys postgrest, supavisor, postgres-meta, and imgproxy to supabase-postgrest, supabase-supavisor, supabase-postgres-meta, and supabase-imgproxy. Updated packages, firestreamContainers, firestreamImages, chart containerRefs keys, compose.nix dependencies and @key refs, profile.nix containerPackages, and docs. Renamed the four container flake-module files and updated flake.nix imports. Deleted the four alias apps in supabase.nix and their header line. Part 2 done. Kept the dev key set as the chart default and kept the equality check against firestreamLib.supabase.devSecrets. Added a NOTES.txt warning that prints only when the dev keys are in use. Extended checks.supabase-render-fidelity to prove a secretRef render drops the warning, drops the key Secret, and holds none of the dev secret values. Documented the production path in docs/supabase.md.

### 2026-10-23

Verification done. nix eval of the four renamed packages passes. The inner package derivations (postgrest-14.1, supavisor-2.9.13, postgres-meta-0.100.0, imgproxy-3.31.4) have null inputSrcs, so they depend on fixed nixpkgs or fetched sources only, never on this repo tree. The rename cannot change their content. checks.supabase-render-fidelity now proves the secretRef render has no key Secret, no dev value (plain or base64), and no NOTES warning, and proves the dev-key render shows the warning. helm template cannot render NOTES.txt, so the check copies the chart, comments NOTES.txt into an ordinary template, and renders it with --show-only. make ci-dry-run and make ci-e2e-dry-run show the new names and no alias apps. make test-e2e-supabase passed (9 tests, 227s). make test-e2e-k8s-supabase passed (14 tests, 371s); the k3d preload built supabase-postgrest-image, supabase-supavisor-image, supabase-postgres-meta-image, and supabase-imgproxy-image with no alias app. Deviation: nix flake check --no-build --all-systems fails on packages.x86_64-linux.nextjs with a stale store path unrelated to supabase; nix build of that package proceeds past that point. This is a pre-existing environment issue outside this ticket's scope, not caused by this change.
