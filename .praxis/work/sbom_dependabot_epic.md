---
id: FIRE-049
title: "Full fleet SBOM and a Dependabot config generated from Nix"
type: plan
status: backlog
priority: high
tags: [ci, epic, nix, sbom]
affects: []
depends-on: [FIRE-050, FIRE-051, FIRE-052, FIRE-053, FIRE-054, FIRE-055, FIRE-056]
---

# Full fleet SBOM and a Dependabot config generated from Nix

The epic. Its `depends-on` lists every child.

## Goal

1. One SBOM covers every image and every dependency, each with the correct ecosystem purl.
2. Nix generates `/.github/dependabot.yml` from the same data.
3. `make ci` runs both.

## Audit findings (2026-10-23)

- `nix/flake-modules/aggregate.nix:21` has a fixed `wanted` list. The fleet SBOM covers 9 of the 27 distinct images.
- The CI `build` phase (`bin/nix/firestream/ci/profile.nix:64`) has a second fixed list.
  - It leaves out `nginx`.
  - Its comment says `odoo-15/16/17`, `postgresql-16` and `redis-7` are aliases, but they are distinct images. Only three pairs are aliases: `odoo`/`odoo-18`, `postgresql`/`postgresql-17` and `redis`/`redis-8`.
- Each per-image SBOM reads only the runtime closure and gives only `pkg:nix` purls. Cargo, npm, pnpm, Mix, Go and JVM dependencies are not visible, so Grype and Trivy find almost nothing.
- Metadata names collide. For example, `supabase-postgres` and `postgresql-17` both use the name `postgresql`, so `merge-sboms` cannot tell them apart.
- Licenses are always `NOASSERTION`. The code sets the top-level license to Apache-2.0, but the repo is MIT.
- There is no root `/.github/dependabot.yml`. GitHub ignores the Bitnami copies under `src/{charts,containers}/.github/`.
- An `npmDepsHash` pins the Next.js `package-lock.json` (one canonical copy plus 3 identical example copies). Any Dependabot bump to it fails the build.
- Grype and Trivy are in the dev shell, and `firestream-vib` has runners for them, but nothing calls them.

## User decisions (2026-10-23)

| Topic | Decision |
|---|---|
| `make ci` and `dependabot.yml` | `make ci` and `make ci-check` write the file first. A required flake check catches drift in GitHub workflows, which call `firestream-ci` directly. |
| Lockfiles pinned by a hash | Remove the hash coupling. Do not leave the lockfiles out. |
| `attest` tier | It stays advisory. A new required coverage check in `verify` enforces completeness. |

## Design rule

Nix reads every in-repo lockfile directly, with no fixed-output hash. Then one lockfile record feeds both the SBOM and Dependabot, and every Dependabot PR builds.

## Parts

- S1 [[FIRE-050]]: one canonical container list.
- S2 [[FIRE-051]]: an `sbomInputs` seam on every builder.
- S3 [[FIRE-052]]: `firestream-vib` makes ecosystem components.
- S4 [[FIRE-053]]: remove the npm hash coupling.
- S5 [[FIRE-054]]: SBOM coverage check.
- S6 [[FIRE-055]]: generate `dependabot.yml`.
- S7 [[FIRE-056]]: wire into `make ci`, the PR gate and the docs.

S1 runs first, then S2. After S2, S3 and S4 can run in parallel. S5 and S6 both need S4. S7 comes last.

Out of scope: a scheduled vulnerability scan phase (Trivy or Grype on images). Open a ticket for it after S7.
