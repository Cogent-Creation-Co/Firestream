---
id: FIRE-038
title: "E2E-E: one-time cleanup of host leftovers"
type: feature
status: done
priority: medium
tags: [e2e]
affects: []
depends-on: [FIRE-035]
---

# E2E-E: one-time cleanup of host leftovers

Epic: [[FIRE-034]]

The user decided (2026-10-23) to remove all old leftovers on this host, including the dangling anonymous volumes that cannot be tied to the harness.

## Steps

1. Run `firestream-e2e-sweep --dry-run`, then `firestream-e2e-sweep`. This removes the `k3d-firestream-e2e-*` networks and any other harness leftovers.
2. List every dangling anonymous volume: `docker volume ls -f dangling=true -f label=com.docker.volume.anonymous`, plus dangling 64-hex names with no label. Record the list in the work log.
3. Remove exactly the listed volumes with `docker volume rm <ids>`. Never use `docker volume prune`, and remove nothing outside the list.

## Acceptance

- The work log holds the full list of removed objects.
- Afterwards, `docker network ls`, `docker volume ls -f dangling=true` and `k3d cluster list` show no harness leftovers and no dangling anonymous volumes.

## Work log

### 2026-10-23

Done by the PM on 2026-10-23. The user approved the removal.

Removed:
- 3 dangling anonymous volumes (label `com.docker.volume.anonymous`), all created at 2026-10-23T08:07:52-07:00. The single timestamp matches the Studio egress helper leak from the [[FIRE-033]] e2e run:
  - `5f13852145b085cc76558a68dd5e0f49da9bcdd78b8d2200853c2cc6e35c1eb5`
  - `8116af1867d5be1d23b093830625dc31fd32bcdd4698fbb67674087baf57cbeb`
  - `c3847befdb1798efeb1c9d1fb769c58bca4a32291bb714b2af8dc6521cfebbc3`
- 2 networks, removed with `firestream-e2e-sweep` (exit 0): `k3d-firestream-e2e-lifecycle-5u750j` and `k3d-firestream-e2e-supabase-rkpzmb`.

Not removed, because they are outside the approved scope: named dangling volumes that the harness does not own (`conceptdb-odoo-e2e_odoo_data`, `conceptdb-odoo-e2e_postgresql_data`, `supabase_edge_runtime_ybsgzrgoikhpuhpformx`). No unlabelled 64-hex dangling volume existed. I did not run a prune command.

Result: 0 `k3d-firestream-e2e-*` networks, 0 dangling anonymous volumes, 0 harness containers.
