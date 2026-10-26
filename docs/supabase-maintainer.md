# Supabase: maintainer notes

This page tells how the Supabase stack is built and how to move it to a new
upstream self-hosted release. The user page is `docs/supabase.md`.

## Where the pins are

The stack follows one upstream self-hosted docker release. The release name
is `release` in `bin/nix/firestream/supabase/default.nix` (now
`self-hosted/v0.8.3`).

| What | File | Pin |
|---|---|---|
| Postgres SQL, `supautils.conf`, extension scripts | `bin/nix/firestream/supabase/default.nix` (`sources.postgres`) | supabase/postgres commit of the image tag in the release compose file |
| Compose volume files (db SQL, Envoy config) | `bin/nix/firestream/supabase/default.nix` (`sources.dockerVolumes`) | supabase/supabase commit of the release, sparse `docker/volumes` |
| supautils, supabase_vault, plan_filter | `bin/nix/firestream/packages/postgresql-extensions/` | version and hash per file |
| Auth (GoTrue) | `bin/nix/firestream/packages/supabase-auth.nix` | `version`, `rev`, `hash`, vendor hash |
| imgproxy | `bin/nix/firestream/packages/imgproxy.nix` | `version`, `hash` |
| Go for Auth and imgproxy | `bin/nix/firestream/packages/go-toolchain.nix` | Go 1.27.3 |
| PostgREST | `src/containers/firestream/postgrest/module.nix` | `pkgs.postgrest` of the nixpkgs pin |
| Envoy | `bin/nix/firestream/packages/envoy/` | the Envoy commit, `deps.json` |
| Realtime | `src/containers/firestream/supabase-realtime/module.nix` | `version`, `hash`, `mix-deps.nix`, `assets/pnpm-lock.yaml` |
| Supavisor | `src/containers/firestream/supavisor/module.nix` | `version`, `hash`, `mix-deps.nix`, NIF `cargoHash` |
| postgres-meta | `src/containers/firestream/postgres-meta/package.nix` | `version`, `hash`, `pnpm-lock.yaml` |
| Storage | `src/containers/firestream/supabase-storage/package.nix` | `version`, `hash`, `pnpm-lock.yaml` |
| Studio | `src/containers/firestream/supabase-studio/package.nix` | supabase/supabase commit, `pnpm-lock.yaml`, the `unusedWasm` list |
| Image tags in the chart | `src/charts/firestream/supabase/values.yaml` (`image.<component>.tag`) | must equal the image versions |
| Development keys | `bin/nix/firestream/supabase/dev-secrets.json` and `secret.supabase.values` in the chart `values.yaml` | must be equal |
| The chart fork | `src/charts/firestream/supabase/Chart.yaml` annotations | supabase-community/supabase-kubernetes commit `8d4b5cda901a3f28535af5c79c5ed95e884e48d9` |

## Sync to a new upstream release

Do the steps in this order. Steps 3, 4, 5 and 6 have a check that fails when
the step is not complete.

1. Read the new release. Open `docker/docker-compose.yml` and
   `docker/.env.example` at the release tag of supabase/supabase. Write down
   the image tag of each service.

2. Bump the shared sources. In `bin/nix/firestream/supabase/default.nix`, set
   `release`, the `sources.postgres` commit (the commit of the
   `supabase/postgres` image tag) and the `sources.dockerVolumes` commit. Set
   each `hash` to `lib.fakeHash`, build, and copy the real hash from the
   error.

3. Sync the database files. Compare the `db` volume mounts in the release
   compose file with the `cp` lines in `postgres.migrations`. Add a line for
   each new mounted file. `logs.sql` stays out, because Logflare is out of
   scope. Then run:

   ```bash
   nix build .#checks.x86_64-linux.firestream-supabase-postgres-migrations
   ```

   The check applies every file to the image's Postgres and asserts the
   schemas, roles and the `_supabase` database.

4. Sync the Envoy config. The image reads
   `docker/volumes/api/envoy` from `sources.dockerVolumes`.
   `src/containers/firestream/supabase-envoy/render-templates.py` changes the
   upstream service addresses into `ENVOY_<SVC>_HOST` and `ENVOY_<SVC>_PORT`.
   It fails the build when an upstream line that it replaces has moved.
   Update its patterns to the new text. The image build also validates the
   rendered config with `envoy --mode validate`.

5. Bump each service. Change the version and hash in the file from the table.
   For postgres-meta, Storage and Studio, also copy the upstream
   `pnpm-lock.yaml`, run `bin/nix/firestream/node/pnpm-lock-json.sh`, and
   `git add` both lock files. The check `firestream-pnpm-lock-json` fails on a
   stale JSON lock file. For Realtime and Supavisor, run
   `bin/nix/firestream/beam/mix-deps.sh`. The sections below give the
   details.

6. Bump the chart. Set `image.<component>.tag` in
   `src/charts/firestream/supabase/values.yaml` to the new image versions.
   `checks.supabase-render-fidelity` fails when a tag or a development key in
   `values.yaml` differs from the flake.

7. Sync the chart templates (optional). Compare the templates of
   supabase-community/supabase-kubernetes at its newest commit with the
   forked commit in `Chart.yaml`. Copy the changes that apply. Then update
   the commit annotation.

8. Verify:

   ```bash
   nix flake check --no-build --all-systems
   nix build .#checks.x86_64-linux.supabase-render-fidelity \
             .#checks.x86_64-linux.firestream-supabase-dev-secrets
   make test-e2e-supabase        # docker compose
   make test-e2e-k8s-supabase    # k3d, SeaweedFS and Supabase
   ```

If you make a new development key set, update `dev-secrets.json` and
`secret.supabase.values` in the chart `values.yaml` together. Make the set
with `nix run .#supabase-keygen -- --output json`.

## Bump Envoy

1. Find the commit of the new Envoy release tag. Set `rev` and `hash` of the
   source in `bin/nix/firestream/packages/envoy/default.nix`. Write the tag in
   the `NOTE:` comment next to the SHA.
2. Read `.bazelversion` in the new tree. If it changed, update the Bazel dist
   archive in `envoy/bazel.nix`.
3. Unpack the new tree, without the patches. Then regenerate `deps.json`:

   ```bash
   python3 bin/nix/firestream/packages/envoy/generate-deps.py ENVOY_SRC \
     --previous bin/nix/firestream/packages/envoy/deps.json \
     -o bin/nix/firestream/packages/envoy/deps.json
   ```

   ENVOY_SRC is the unpacked tree. The script reads both
   `repository_locations.bzl` files and `Cargo.Bazel.lock`. It writes one
   fetch for each archive, Go module and Rust component. `--previous` copies
   the hashes that did not change, so the script downloads only new files.
4. Rebase `envoy/patches/envoy-nix-toolchains.patch` and
   `envoy/patches/cel-cpp-antlr.patch` onto the new tree.
5. Run `bazel build --nobuild //source/exe:envoy-static` offline in the
   derivation build environment. Repeat until no fetch is missing. A missing
   fetch is an archive that the generator leaves out, or a prebuilt
   toolchain that the patch must replace.
6. Set the version in `src/containers/firestream/supabase-envoy/options.nix`.
7. Verify:

   ```bash
   nix build .#checks.x86_64-linux.supabase-envoy-deps
   nix build .#supabase-envoy
   ```

   The check fails when `deps.json` differs from the generator output. The
   image build runs `envoy --mode validate` on the rendered config.

## Bump the Mix deps

Realtime and Supavisor fetch each Mix dependency on its own. The file
`mix-deps.nix` next to each `module.nix` holds the fetches.

1. Bump `version`, `rev` and `hash` of the app source.
2. Run the generator. It reads `mix.lock` from the new source:

   ```bash
   bin/nix/firestream/beam/mix-deps.sh
   ```

   A Hex package gets the outer checksum from the lock. A Git dependency gets
   `fetchFromGitHub` at the commit from the lock. The script downloads only
   the packages that are new or changed.
3. For Supavisor, reset the NIF `cargoHash` in `module.nix` to
   `lib.fakeHash`. Build, and copy the real hash from the error.
4. Verify:

   ```bash
   nix build .#checks.x86_64-linux.supabase-realtime-mix-deps \
             .#checks.x86_64-linux.supabase-supavisor-mix-deps
   ```

   Each check runs the generator offline and fails on a stale file.

## Bump a pnpm lockfile

The vendored pnpm2nix reads a JSON copy of each `pnpm-lock.yaml`. A YAML
parse at evaluation time needs import-from-derivation.

1. Copy the upstream `pnpm-lock.yaml`. For the Realtime assets and the
   Realtime Tailwind, make the file with `pnpm import` from the upstream
   `package-lock.json`.
2. Write the JSON files:

   ```bash
   bin/nix/firestream/node/pnpm-lock-json.sh
   ```

3. `git add` the YAML file and the JSON file.
4. Verify with `nix build .#checks.x86_64-linux.firestream-pnpm-lock-json`.
   The check fails on a stale JSON file.

pnpm2nix fetches each tarball with the integrity value from the lockfile.
Its platform filter drops the npm packages that hold only a binary.
`excludePackages` drops a named package in the same way.

## Bump a native Node tool

The Node images do not use the prebuilt binary of an npm package. A Nix
derivation in `bin/nix/firestream/node/native/` builds each one from its
GitHub source:

| File | Output | Used by |
|---|---|---|
| `next-swc.nix` | `@next/swc` | Studio build |
| `tailwindcss-oxide.nix` | `@tailwindcss/oxide` | Studio build |
| `lightningcss.nix` | `lightningcss` | Studio build |
| `libpg-query.nix` | `libpg-query.wasm` and `.js` | postgres-meta, Studio |
| `source-map-mappings/` | source-map `mappings.wasm` | Storage |

The oxfmt binding of postgres-meta is in
`src/containers/firestream/postgres-meta/package.nix`.

When the npm version of one of these packages changes:

1. Find the GitHub commit of the new npm version. Set `rev` and `hash`, and
   write the tag in the `NOTE:` comment.
2. Set the Rust toolchain to the one that `rust-toolchain.toml` pins at that
   commit. Change the channel URL and its `sha256`.
3. Reset `cargoHash` to `lib.fakeHash`. Build, and copy the real hash from
   the error. `source-map-mappings` uses its own `Cargo.lock`; copy the new
   lock from the upstream tree.
4. Build the image that uses the tool. The Studio build runs
   `find-machine-code.cjs` on `node_modules` before `next build`. It fails
   when a new machine-code file appears.

## The binary scan allowlist

The check `checks.<sys>.supabase-no-prebuilt-binaries` is in the `verify` CI
phase. It reads the app output of each image. A file with an ELF, Mach-O, PE
or WebAssembly header fails the check, unless an allowlist entry covers it.
The allowlist is in `nix/flake-modules/supabase-pinning.nix`.
`docs/nix-pinning-policy.md` gives the full rules.

- Add an entry only for a file that a Nix derivation builds from a pinned
  source.
- `pattern` must match the full path of the file in the output.
- `builtBy` must be the derivation that builds the file, not its name.
- `reason` tells in one line how the derivation builds the file.
- Use `allowAll` only for the app's own source build. Do not use it for a
  tree of registry packages.
- An entry that matches no file fails the check. Remove the entry when the
  file goes.
- A temporary entry for a prebuilt binary needs a `TODO(<ticket>)` comment
  with a real ticket.

Run the check after each bump:

```bash
nix build .#checks.x86_64-linux.supabase-no-prebuilt-binaries
```

## The reproducibility check

The `repro-supabase-fods` CI phase runs in the `e2e` mode
(`make ci-e2e`). It is advisory. It runs
`bin/nix/firestream/supabase/rebuild-fods.py` on the ten Supabase images.

- The script reads the derivation closure of each image with
  `nix derivation show --recursive`. It keeps each fixed-output derivation
  (FOD).
- It removes each FOD that cache.nixos.org serves. That is the trusted
  nixpkgs base.
- It runs `nix build --rebuild` on each FOD that remains. A hash mismatch
  or a failed fetch prints `DRIFT <drv>`, and the script exits 1.

Run it on one image, or on a subset:

```bash
bin/nix/firestream/supabase/rebuild-fods.py --list .#supabase-auth
bin/nix/firestream/supabase/rebuild-fods.py --match 'go-modules|vendor' \
  .#supabase-auth .#supabase-imgproxy
bin/nix/firestream/supabase/rebuild-fods.py --limit 50 .#supabase-studio
```

When a FOD drifts, find its source. A moved upstream file needs a new pin. A
vendor FOD (Go `vendorHash`, `cargoHash`) that drifts needs a fix in the
build, not a new hash.

The script does not see a fetch at evaluation time, for example
`builtins.fetchurl` of a Rust channel manifest. Nix checks the hash of such
a fetch at each evaluation.

## Known gaps against upstream v0.8.3

- PostgREST is 14.1 (`pkgs.postgrest` of the nixpkgs pin); upstream runs 14.18.
- The Realtime build has no `pgdelta`.
- pg_graphql is off by default (upstream migration `20260421000000`).

## Source provenance notes

- libpg-query 17.6.0 (postgres-meta and Studio) builds from
  constructive-io/libpg-query-node commit `727c0826`, branch
  `fix/increase-memory`. The npm `gitHead` of 17.6.0 (`7a7825a4`) is on no
  GitHub branch, and npm published 17.6.0 three minutes after `727c0826`.
  The build passes the upstream v17 tests (41/41). It gives the same parse
  trees as the npm files for 50 statements.
- The Studio build deletes the prebuilt WebAssembly files in `unusedWasm`
  before `next build`. A trace shows that the build compiles none of them.
  When you bump Studio, the build stops if a listed file is gone. Then trace
  the new build again, and update the list.

## Build costs

- Envoy 1.39.1 compiles from source with Bazel, offline. The package is
  `bin/nix/firestream/packages/envoy/`. It takes about one hour with 12
  Bazel jobs. Give it 12 cores or fewer on a 32 GB machine.
- `envoy/deps.json` pins each Bazel archive, Go module and Rust component.
  See "Bump Envoy" above.
- The Studio closure is about 810 MiB.
- Auth and imgproxy need a newer Go than the nixpkgs pin. They use
  `firestream.packages.goToolchain` (Go 1.27.3).

## Library pieces

| Piece | File | Use |
|---|---|---|
| `firestream.supabase` | `bin/nix/firestream/supabase/default.nix` | upstream pins, migrations, `devSecrets`, `keygen` |
| `supabase-keygen` | `bin/nix/firestream/supabase/keygen.py`, `nix run .#supabase-keygen` | a consistent key set: `--output env`, `json` or `k8s-secret` |
| `mkMixRelease` | `bin/nix/firestream/beam/mkMixRelease.nix` | an Elixir release from a Mix project |
| `mkBeamContainerModule` | `bin/nix/firestream/containers/beam.nix` | the container factory for Elixir releases (Realtime, Supavisor) |
| pnpm2nix (vendored) | `bin/nix/firestream/node/pnpm2nix.nix`, `bin/nix/firestream/node/pnpm2nix/` | Node.js apps from a pnpm lock file, without import-from-derivation |
| `goToolchain` | `bin/nix/firestream/packages/go-toolchain.nix` | Go 1.27.3 |

## The compose stack and the chart

- The compose stack is declared in
  `src/containers/firestream/supabase-envoy/compose.nix` on the gateway image.
  `nix/flake-modules/supabase.nix` publishes it as `supabase-compose`,
  `supabase-up` and `supabase-down`.
- The chart is `src/charts/firestream/supabase/` (typed options in its
  `nix/`). `nix/flake-modules/charts/supabase.nix` injects the ten images and
  the development keys. The stack `firestreamStacks.supabase` is
  `[ seaweedfs supabase ]`.
- The k8s e2e harness deploys the members of a bundle stack that has the
  chart's own name before the chart (`stack_prerequisites` in
  `src/lib/rust/firestream-e2e-k8s/src/harness.rs`).
