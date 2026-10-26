---
id: FIRE-013
title: "Spike: Supabase Storage on SeaweedFS"
type: research
status: done
priority: high
tags: [phase-0, spike, supabase]
affects: []
depends-on: []
---

# Spike: Supabase Storage on SeaweedFS

Epic: [[FIRE-009]]

## Goal

Find out if Supabase Storage API (v1.81.0) works with the firestream
SeaweedFS S3 gateway as its backend. No one has reported results for this
combination.

## Questions

- Do TUS resumable uploads work? They use multipart upload and object
  tagging.
- Do presigned upload and download URLs work?
- Do copy and move of objects work?
- Does `STORAGE_VERSIONING_ENABLED` work, or must it stay off?
- Which settings are necessary: `STORAGE_BACKEND=s3`, `GLOBAL_S3_ENDPOINT`,
  `GLOBAL_S3_FORCE_PATH_STYLE`, `GLOBAL_S3_PROTOCOL`, and the tagging
  switch?
- Can Storage use the `S3_LOCAL_*` values that the SeaweedFS chart already
  gives to Spark and Airflow?

## Scope

- A throwaway test against the `seaweedfs` compose stack. The Storage
  process can run outside a container for this test.

## Exit

Write the result in the ticket log: "go", "go with changes" or "no-go and
why". List the necessary settings and the features to turn off. FIRE-023
uses this result.

## Result

**Verdict: go with changes.** Supabase Storage API v1.81.0 works on the Firestream SeaweedFS S3 gateway (`weed` 4.36-17-g5797fb24e, auth on, chart credentials). All features in scope pass. One SeaweedFS difference exists in a TUS edge case. Versioning works and can stay on, but one Storage bug affects TUS on versioned buckets.

Scratch: `/tmp/firestream-supabase-spikes/FIRE-013/` — `storage.nix` (Nix build of Storage), `s3.json` (SeaweedFS identity), `env.sh`, `test_storage.py`, `test_resume_versioning.py`, `head_after.py`.

### Test setup

- SeaweedFS: the Firestream `weed` package (`bin/nix/firestream/packages/seaweedfs-weed.nix`) as `weed server -master -volume -filer -s3` with `-s3.config` that holds identity `firestream`/`firestream-secret` (actions Admin, Read, Write, List, Tagging). Anonymous S3 access returns 403. The compose stack (`.#seaweedfs-compose`) runs without S3 auth, so the spike used the binary with the chart's auth.
- Storage: built by Nix (see below), run as a process against the FIRE-012 database (`supabase_storage_admin`). Storage ran its own schema migrations at start.
- Control: the same tests against nixpkgs MinIO.

### Results (SeaweedFS)

| Feature | Result |
|---|---|
| Standard upload and download | pass |
| TUS create, 2 chunks of 6 MiB, download byte-exact | pass. S3 multipart with 2 parts (ETag `…-2`) |
| TUS object tagging | pass. The `.info` object carries `X-Amz-Tagging-Tus-Completed: true` |
| TUS resume (HEAD mid-upload returns offset 6291456, then finish) | pass |
| Signed download URL (no auth header) | pass |
| Signed upload URL (no auth header), then read | pass |
| Copy, move (source gone, destination intact) | pass |
| List | pass |
| Bucket versioning: enable, two uploads, list two versions, read old version by `versionId` | pass |

Result: 15/16 checks in `test_storage.py` and 8/8 in `test_resume_versioning.py`. The one failure is the next item.

### Problems

1. **HEAD after a finished TUS upload.** SeaweedFS returns `Upload-Offset: 0`. MinIO returns the full length. A later PATCH on that upload returns 500 ("multipart upload does not exist"). The stored object stays intact. A client that resumes a finished upload from a stored fingerprint can see offset 0 and fail. `tus-js-client` keeps fingerprints by default. Workaround: set `removeFingerprintOnSuccess: true` in clients. Report it upstream to SeaweedFS (ListParts on a completed upload).
2. **TUS upsert on a versioned bucket.** A second TUS upload with `x-upsert: true` to an existing key fails with 409 (`idx_objects_current_version`). The same happens on MinIO, so it is a Storage defect, not a SeaweedFS defect. Standard uploads with upsert on versioned buckets work.
3. Test artifact, not a defect: with `-volume.max=8` and a 32 MB volume limit, SeaweedFS ran out of writable volumes and Storage returned 500. Give the chart enough volume slots and disk.

### Settings

- `STORAGE_BACKEND=s3`
- `GLOBAL_S3_BUCKET=<bucket>` (must exist; the spike created it with `weed shell s3.bucket.create`)
- `GLOBAL_S3_ENDPOINT=http://<seaweedfs>:8333`
- `GLOBAL_S3_FORCE_PATH_STYLE=true` (necessary)
- `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `REGION=us-east-1`, `STORAGE_S3_REGION=us-east-1`
- `TUS_ALLOW_S3_TAGS`: keep the default (true). Tagging works.
- `STORAGE_VERSIONING_ENABLED=true` works. Keep it off by default until problem 2 is fixed, or document it.
- `GLOBAL_S3_PROTOCOL`: v1.81.0 does not read it. The endpoint URL scheme decides. Drop it.
- CRC32 request checksums (AWS SDK default) work. `STORAGE_S3_REQUEST_CHECKSUM_CALCULATION` is not necessary.
- Others: `DATABASE_URL`, `ANON_KEY`, `SERVICE_KEY`, `AUTH_JWT_SECRET`, `PGRST_JWT_SECRET`, `TENANT_ID`, `FILE_SIZE_LIMIT`, `STORAGE_PUBLIC_URL`, `REQUEST_ALLOW_X_FORWARDED_PATH=true`, `ENABLE_IMAGE_TRANSFORMATION`, `IMGPROXY_URL`, `S3_PROTOCOL_ACCESS_KEY_ID`, `S3_PROTOCOL_ACCESS_KEY_SECRET`, `SERVER_PORT`.

### `S3_LOCAL_*` reuse

Yes, the values work. The names differ, so map them in the chart: `GLOBAL_S3_ENDPOINT=$(S3_LOCAL_ENDPOINT_URL)`, `AWS_ACCESS_KEY_ID=$(S3_LOCAL_ACCESS_KEY_ID)`, `AWS_SECRET_ACCESS_KEY=$(S3_LOCAL_SECRET_ACCESS_KEY)`, `GLOBAL_S3_BUCKET=$(S3_LOCAL_BUCKET_NAME)`, `REGION=$(S3_LOCAL_DEFAULT_REGION)`. Storage writes keys as `<TENANT_ID>/<bucket>/<name>/<version>`, so it can share the `firestream` bucket. A dedicated bucket (for example `supabase-storage` in SeaweedFS `createBuckets`) is cleaner for quotas and lifecycle. Recommend it.

### Nix build of Storage (input for FIRE-023)

- `buildNpmPackage` with `nodejs_24` (24.11.1). `engines` asks for node `>=24`, npm `>=11.12.1`. nixpkgs npm is 11.6.2, and `.npmrc` sets `engine-strict=true`. Fix: `npmFlags = [ "--engine-strict=false" ]`.
- `pg-boss` comes from a GitHub source spec (`github:supabase/pg-boss#feat/exactly_once`). npm prepares it inside its cache. Fix: `makeCacheWritable = true`.
- Native addons: `fs-xattr` rebuilds from source. `@datadog/pprof` ships prebuilt `.node` binaries for many platforms. Delete the foreign prebuilds or rebuild it, if the "all Nix" rule covers vendored binaries.
- Prune with a writable `npm_config_cache` in `installPhase`.
- npm deps hash `sha256-BvpPcnVwXK6ZZycZnO7ALwCZ8gYwZg9ZAKJ3UIsfUas=`. Build 0:42. Output 569 MB, closure 1.2 GiB. Trim it in FIRE-023.
- Start: `node dist/start/server.js` from the package directory.

### Impact on downstream tickets

- FIRE-023: use the build method and settings above. Map `S3_LOCAL_*`. Default versioning off. Document the client workaround for problem 1.
- The SeaweedFS chart needs a bucket for Storage and enough volume slots. The compose stack has no S3 auth, so compose-based Supabase tests need an `-s3.config` (or a test that accepts anonymous access).

## Work log

### 2026-10-22

Spike done. Verdict: go with changes. All in-scope features pass on SeaweedFS. One TUS HEAD edge case differs from MinIO. One Storage TUS+versioning defect. See the Result section.
