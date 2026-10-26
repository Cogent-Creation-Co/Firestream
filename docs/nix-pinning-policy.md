# Nix pinning policy

This policy applies to every input of a Firestream image. The Supabase images
are the first images that a check enforces it on.

## The rule

Build each image from source. Pin each input to an exact, immutable revision
and a content hash. Do not ship a prebuilt binary. Do not run a prebuilt
binary at build time, if a source build of it is possible.

## Allowed inputs

| Input | Pin |
|---|---|
| A GitHub repository | `fetchFromGitHub` with a 40-character commit SHA and a `hash`. |
| Another Git or HTTP source | A commit SHA or a versioned URL, and a `hash`. |
| A registry package (npm, Hex, crates.io, Go proxy) | The upstream lockfile. Nix fetches each package on its own, with the hash from the lockfile. |
| A nixpkgs package | The `nixpkgs` input in `flake.lock`. |
| A Rust toolchain | The `fenix` input in `flake.lock`. |

A registry package is source only. If the package contains a binary, remove
the binary. Then build that binary from its GitHub source in a Nix
derivation, if the app needs it.

## Forbidden inputs

- **A prebuilt binary from outside nixpkgs.** This includes an ELF, Mach-O or
  PE file, a `.node` addon, a `.wasm` module, and a `prebuilds/` directory.
  A GitHub release asset is a prebuilt binary too.
- **A moving ref.** A branch name and a tag can move. Use the commit SHA. You
  can write the tag in a comment next to the SHA.
- **A monolithic package-manager fetch.** An example is one fixed-output
  derivation that runs `bazel fetch`, `mix deps.get` or `npm install` and
  hashes the result. Do not use one when a per-package fetch exists. Such a
  fetch drifts: the same command gives a new hash after upstream changes.

## The trusted base

You can trust two inputs without a source build in this repository:

- **nixpkgs.** This includes its bootstrap seeds and the binaries that
  `cache.nixos.org` serves for it.
- **fenix.** This includes its Rust toolchains.

An override of a nixpkgs package is not part of the trusted base. Its
output is a Firestream build, and the check scans it.

## The check

The check `checks.<system>.supabase-no-prebuilt-binaries` enforces this
policy for the Supabase images. It runs on Linux only.

```bash
nix build .#checks.x86_64-linux.supabase-no-prebuilt-binaries
```

The check reads the app output of each Supabase image:

- the `node_modules` trees of Storage, postgres-meta and Studio;
- the release trees of Realtime and Supavisor;
- the Postgres extensions that Firestream builds;
- the Auth, imgproxy, PostgREST and Envoy builds.

It does not read the base image or other nixpkgs store paths.

The check reads the first bytes of each file. It resolves each symlink, and
it reads each real file one time. A file is a binary if it starts with an
ELF, Mach-O, PE or WebAssembly header.

A binary passes only if an allowlist entry covers it. The check fails for a
binary with no entry. It also fails for an entry that matches no file. On a
failure, it prints a table of the image, the derivation, the kind of binary,
and the path.

The allowlist is in `nix/flake-modules/supabase-pinning.nix`. The scanner
is `bin/nix/firestream/supabase/scan-binaries.py`.

## How to add an exception

Add an exception only for a binary that a Nix derivation builds from source.

1. Make sure a Nix derivation builds the binary from a pinned source.
2. Add an entry to the `allow` list of the target in
   `nix/flake-modules/supabase-pinning.nix`. Give these fields:
   - `pattern`: a regular expression. It must match the full path of the
     file in the output.
   - `builtBy`: the derivation that built the file. Use the derivation, not
     its name. Evaluation fails if `builtBy` is not a derivation.
   - `reason`: one line that tells how the derivation builds the file.
3. If the target output is the app's own source build, set `allowAll` to a
   reason. Do not use `allowAll` for a tree of registry packages.

A temporary exception is different. A temporary exception lets a prebuilt
binary stay until a ticket removes it. Add a `TODO(<ticket>)` comment above
the allowlist entry. The ticket must exist in the tracker. Remove the entry
and the comment when the ticket closes.

## The CI status

The check is in the `verify` CI phase. A new prebuilt binary or a stale
allowlist entry fails `make ci-check`.

The `repro-supabase-fods` phase of the `e2e` mode builds each remaining
fixed-output derivation of the Supabase images again. It is advisory. See
`docs/supabase-maintainer.md`.
