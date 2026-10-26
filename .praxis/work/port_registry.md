---
id: FIRE-060
title: "One port registry for every stack and service"
type: feature
status: active
priority: high
tags: [compose, e2e, nix]
affects: []
depends-on: []
---

# One port registry for every stack and service

Found in [[FIRE-039]].

## Problem

Each compose stack publishes its container ports on the host at `containerPort + compose.hostPortOffset`. The offsets are 20000 to 42000. So most host ports fall in the Linux ephemeral range, 32768 to 60999, where an outgoing connection can hold one for a moment. A Supabase e2e run failed its port preflight this way. FIRE-039 added a 20-second grace period as a mitigation.

## The user's decision (2026-10-24)

"All ports for all services should be declared in one place in Firestream, where they don't overlap. Then each service should lay claim to whatever canonical one it usually does."

When one service runs in several stacks (for example postgres in Airflow, Odoo, Superset, JupyterHub, Next.js and Supabase), the registry has one entry for each pair of stack and service:
- The service's first owner gets the canonical host port. For example, the postgresql stack gets 5432.
- Each other owner gets a fixed host port that the registry chooses.
- Every stack can run at the same time.
- No offsets remain anywhere.

## Scope

1. **The registry:** one file, for example `bin/nix/firestream/ports.nix`. It maps each published container port of each compose service in each stack to a host port, including the healthd port 9180 where a service publishes it. The file says which stack owns each canonical port.
2. **Evaluation-time rules,** each failing with a clear message that names the entries:
   - no host port is used twice;
   - every host port is above 1023 and below 32768;
   - every published port of every compose stack has a registry entry;
   - no registry entry is stale.
3. **`compose.nix`:** read the host ports from the registry. Remove `compose.hostPortOffset` from the schema in `bin/nix/firestream/containers/eval-container.nix` and from every container's options or compose file. Setting it must fail with a message that points to the registry.
4. **e2e:** the harness still reads the host ports from the compose package. Remove the 20-second grace period from the port preflight in `src/lib/rust/firestream/tests/e2e/harness.rs`; ports below 32768 do not need it. A port that something holds must still fail the preflight at once, with the holder's name.
5. **A check:** `checks.<sys>.firestream-port-registry`, with assertions for each rule.
6. **Docs:** a port table in `docs/` that is generated from the registry, or checked against it. Update the port sections of `CLAUDE.md`, `docs/supabase.md` and `docs/firestream-supported-app.md`.

## Acceptance

- The registry check passes, and each rule fails when you break it (temporary edits, then revert).
- Every compose stack's YAML changes only in its published host ports, and each new host port equals the registry entry. Prove it with a same-tree comparison.
- Start all compose stacks at the same time. Every service gets its port, and there is no conflict.
- `make test-e2e` passes for all stacks with the host snapshot unchanged, and `make test-e2e-supabase` passes 5 times in a row without the grace period.
- `nix flake check --no-build --all-systems` passes.

## Work log

### 2026-10-24

Registry added: `bin/nix/firestream/ports.nix` (table) and `bin/nix/firestream/ports-lib.nix` (rules). `compose.nix` reads every host port from it. `compose.hostPortOffset` is removed from the schema and from all container files. Service `ports` are now container port integers.

Same-tree proof: the 31 compose YAMLs were rendered before and after the change. All 90 published ports keep their container port, and each new host port equals its registry row. The only other change is the Supabase public URL (`localhost:50000` to `localhost:8000`), because it embeds the gateway host port. `projectName`, `hostPorts` order and `healthHostPort` map one to one.

`checks.x86_64-linux.firestream-port-registry` builds.

### 2026-10-24

Failure proof, by temporary edits that were then reverted. Each edit makes the check fail at evaluation time with a message that names the rows:
- A duplicate: `host port 11180 is used twice: airflow/airflow:8080, airflow/airflow:9180`.
- Out of range: `host port 40000 of postgresql/postgresql:9180 is outside 1024-32767`.
- A missing row: `redis/redis:9180 is published but has no row`.
- A stale row: `row redis/redis:7000 -> 13000 is stale: no compose stack publishes it`.
- A stale or bad `stackOf` key, and a bad row shape.
- A changed `docs/ports.md` table. The message prints the correct table.
- `compose.hostPortOffset = 1000` on redis: the error points to the registry.
The check also holds 10 permanent negative cases, so a rule that accepts everything turns it red.

### 2026-10-24

e2e harness: `resolve_stack` reads `passthru.portMap`. The probes look up a host port by compose service and container port, so no host port number stays in Rust. The port preflight has no grace period now. A held port fails at once and names the container or, through `ss`, the process. `cargo test -p firestream --test e2e`: 2 pass, 9 ignored.

The k3d harness is not affected. It uses kernel-chosen ports (`pick_ephemeral_port`) and port-forwards, and reads no compose port.

Docs: new `docs/ports.md` with the table. Updated `CLAUDE.md`, `docs/supabase.md`, `docs/firestream-supported-app.md` and the three `examples/*-docker-compose` READMEs and `up.sh` scripts. `docs/supabase-maintainer.md` names no host port and does not change.

`nix flake check --no-build --all-systems` passes. The registry check evaluates on all four systems.

### 2026-10-24

All-stacks proof, partial. A script started every compose stack, one after the other, and left each one up. Eight stacks ran at the same time: airflow, postgresql, redis, kafka, spark, jupyterhub, superset and odoo (17 containers). Each `up` exit 0. Each healthcheck is healthy. No host port is published twice, and docker reports no bind conflict.

Claude Code stopped the script while supabase built, because the host was low on memory. The script was not started again. The eight projects were removed with `down -v --remove-orphans`; no `fire060-*` container, volume or network stays.

Not run yet: the other 13 stacks together, `make test-e2e` and 5x `make test-e2e-supabase`.
