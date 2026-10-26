---
id: FIRE-022
title: "imgproxy image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: []
---

# imgproxy image

Epic: [[FIRE-009]]

## Goal

Package imgproxy as a firestream image for Supabase Storage image
transforms.

## Scope

- Use `pkgs.imgproxy` (3.31.3 in our pin; upstream pins v3.31.4).
- Add `src/containers/firestream/imgproxy/{options.nix,module.nix}`, the
  flake-module, the `flake.nix` import and the CI profile entry.
- Type the env that Storage uses: `IMGPROXY_BIND`,
  `IMGPROXY_LOCAL_FILESYSTEM_ROOT`, `IMGPROXY_USE_ETAG`,
  `IMGPROXY_ENABLE_WEBP_DETECTION`, and the S3 source settings.
- Use only open-source features. imgproxy Pro features are commercial.

## Acceptance

- The image builds and `GET /health` answers.
- A resize request returns a resized image.

## Work log

### 2026-10-22

Done. The imgproxy image builds at the upstream version 3.31.4.

Files:
- `bin/nix/firestream/packages/imgproxy.nix`: the nixpkgs expression, overridden to v3.31.4 (`firestream.packages.imgproxy`).
- `bin/nix/firestream/packages/go-toolchain.nix` and `go-no-vendor-checks-1.27.patch`: Go 1.27.3 (`firestream.packages.goToolchain.{go,buildGoModule}`). imgproxy 3.31.x needs go >= 1.26 and supabase/auth v2.197.0 needs go >= 1.27. The pin has go 1.25.5. The toolchain reuses the pin's `go_1_25` expression, bootstrapped by go 1.25. One nixpkgs patch (vendor checks) did not apply, so it is rebased onto 1.27.
- `bin/nix/firestream/packages/default.nix`: registers `imgproxy` and `goToolchain`.
- `src/containers/firestream/imgproxy/{options.nix,module.nix}`: container `imgproxy`, image `firestream-supabase-imgproxy:3.31.4`.
- `nix/flake-modules/containers/imgproxy.nix`, the `flake.nix` import, and the `containerPackages` entry.

Env: `IMGPROXY_BIND=:5001`, `IMGPROXY_LOCAL_FILESYSTEM_ROOT=/`, `IMGPROXY_USE_ETAG=true`, `IMGPROXY_AUTO_WEBP=true`, `IMGPROXY_MAX_SRC_RESOLUTION=16.8`. The S3 source settings (`IMGPROXY_USE_S3`, `IMGPROXY_S3_ENDPOINT`, `IMGPROXY_S3_REGION`, `AWS_*`) exist and are off. `IMGPROXY_AUTO_WEBP` replaces the deprecated `IMGPROXY_ENABLE_WEBP_DETECTION` that the ticket names; upstream compose uses `IMGPROXY_AUTO_WEBP`. Only the open-source build is present.

Verification (docker, `--read-only`, a PNG mounted at `/var/lib/storage`):
- `GET /health`: 200. healthd `GET :9180/readyz` (`imgproxy health`): 200.
- `GET /insecure/rs:fit:200:150/plain/local:///var/lib/storage/test.png@png`: 200, `image/png`. The input is 800x600 and the output is 200x150.
- `imgproxy version` prints 3.31.4. The log has no errors.

For Phase 3: port 5001. With the file backend, mount the Storage volume at the same path in imgproxy. With the S3 backend, Storage sends a signed HTTP URL (to confirm in [[FIRE-023]]). Storage sets `IMGPROXY_URL=http://imgproxy:5001`.
