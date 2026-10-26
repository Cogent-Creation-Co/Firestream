---
id: FIRE-059
title: "Odoo image backup job finds gzip"
type: feature
status: done
priority: high
tags: [e2e, odoo]
affects: []
depends-on: []
---

# Odoo image backup job finds gzip

Epic: [[FIRE-034]]. Found in [[FIRE-039]].

## Problem

`make test-e2e-k8s-odoo-backup` fails. The `odoodump` backup job stops with `gzip: command not found`. The odoo image has no `gzip` on its PATH. The job passed when the odoo backup e2e was added ([[FIRE-005]], [[FIRE-007]]), so this is probably a regression.

## Scope

- Find the cause. Which change removed `gzip` from the image, or from the PATH of the backup job? Compare the image's `runtimeBinDeps` and the job's environment with the version that passed. Record the cause in the log.
- Fix it at the cause, for example by adding `pkgs.gzip` to the odoo image's runtime binaries or by fixing the job's PATH. The archive format in `src/containers/firestream/odoo/scripts/helpers.sh` (`odoo_dump`, `odoo_restore`) stays the same.
- Add a check that the odoo image's backup tools are present (`gzip`, `tar`, `pg_dump`, `psql`), so a later change cannot remove one without notice.

## Acceptance

- `make test-e2e-k8s-odoo-backup` passes, and the host snapshot is unchanged after the run.
- `checks.firestream-odoo-backup` and `odoo-render-fidelity` pass.
- `nix flake check --no-build --all-systems` passes.

## Work log

### 2026-10-24

Cause 1 (gzip): this is not a regression. `runtimeBinDeps` never held `gzip`. FIRE-005 (c848075a) added `gnutar` and `tar -czhf`. `tar -z` runs `gzip` from PATH. The image PATH holds only the `/bin` of its listed contents, so the gzip in the closure was not on PATH. The e2e never reached the Job before: its cluster name had 33 characters until FIRE-039 fixed it.

Cause 2 (found after fix 1): the restore Job refused the archive with "the set is Odoo unknown and this image is Odoo 18.0". Commit 37eba36f (FIRE-008) moved the restore to `firestream-odoo-restore`. That wrapper loads `odoo-env.sh`, so it knows `ODOO_VERSION`. The shell backup Job never loads that env, so the manifest records `unknown`. The old shell restore skipped the version check, because its `ODOO_VERSION` was also empty.

Cause 3 (test defect): `kubectl exec` does not see `ODOO_DATA_DIR`, because it is an image env default. The e2e wrote the sentinel to `/filestore/...` outside the volume.

Fix:
- `module.nix`: add `gzip` to `runtimeBinDeps`.
- `helpers.sh`: new `odoo_load_image_env`. `odoo_dump` calls it. The archive format does not change.
- `odoo_backup.rs`: `odoo_bash` calls `odoo_load_image_env`.
- New check `checks.firestream-odoo-backup-tools` in `nix/flake-modules/checks.nix`. It asserts `bash gzip tar pg_dump psql aws pgrep mktemp` in the `/bin` of the chart image's `runtimeEnv`.

Verification:
- The image before the fix had `/bin/{tar,pg_dump,psql,aws}` and no `/bin/gzip`.
- `firestream-odoo-backup-tools`, `firestream-odoo-backup` (22 tests), `odoo-render-fidelity`, `odoo-backup-render` and `firestream-odoo-module-sync` pass.
- `make test-e2e-k8s-odoo-backup` with STRICT=1: 1 passed in 376 s. The round-trip is OK.
- The host snapshot has the same object names before and after. Three tags point to reloaded image ids. No dangling image is new.
- One KEEP=1 diagnosis run (5vovrr): `make e2e-sweep RUN=5vovrr` removed it.
- `nix flake check --no-build --all-systems` passes.

Open risk: archives that the CronJob made before this fix record `unknown`. A restore of one needs `ODOO_RESTORE_FORCE=yes`.
