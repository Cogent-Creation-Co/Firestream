# src/util virtual workspace — migration notes

This directory was restructured from two isolated single-crate workspaces
(`otel-cli` and `firestream-nix-build`, each with its own `Cargo.lock`) into a
single virtual Cargo workspace with a unified `Cargo.lock` (Phase 1 of the
`src/util/` build-toolkit extraction; see `docs/oxi-ci-migration-scope.md`).

## Direct-dep unification

Shared deps are now declared once in `[workspace.dependencies]` at
`src/util/Cargo.toml`. Members inherit via `dep = { workspace = true,
features = [...] }` to add per-crate features without re-pinning versions.

Unified deps: `clap`, `tokio`, `async-trait`, `futures`, `serde`,
`serde_json`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`,
`chrono`, `tempfile`, `nix`, `assert_cmd`, `predicates`.

Crate-specific deps remain in each member's `[dependencies]`:

- `otel-cli`: `clap_complete`, `opentelemetry-proto`, `tonic`,
  `tonic-prost`, `prost`, `reqwest`, `axum`, `tower`, `hyper`,
  `ratatui`, `crossterm`, `regex`, `hex`, `bytes`, `humantime`,
  `whoami`, `rand`, `url`, `rustls`, `rustls-pemfile`,
  `webpki-roots`, `libc` (macOS), `walkdir` (dev), `wiremock` (dev),
  `tokio-stream` (dev)
- `firestream-nix-build`: `quick-xml`, `shlex`, `otel-cli` (path dep)

## `cargo tree -d` duplicates (transitive, accepted)

The unified lockfile contains these duplicate transitive deps. All are
introduced by independent upstream crates pinning different majors of
common libraries; none originate from our direct deps or from
unification. They existed in each isolated lockfile previously as well.

```
axum v0.7.9      ← otel-cli (direct)
axum v0.8.9      ← via tonic 0.14
axum-core v0.4.5 ← via axum 0.7
axum-core v0.5.6 ← via axum 0.8
getrandom v0.2.17 ← via rand 0.8 / ring
getrandom v0.3.4  ← via rand 0.9 (opentelemetry_sdk)
getrandom v0.4.2  ← via tempfile 3.27
hashbrown v0.15.5 ← via lru → ratatui
hashbrown v0.17.1 ← via indexmap 2.14
itertools v0.13.0 ← via ratatui
itertools v0.14.0 ← via prost-derive
linux-raw-sys v0.4.15 ← via rustix 0.38
linux-raw-sys v0.12.1 ← via rustix 1.1
matchit v0.7.3   ← via axum 0.7
matchit v0.8.4   ← via axum 0.8
rand v0.8.6      ← otel-cli (direct)
rand v0.9.4      ← via opentelemetry_sdk
rand_chacha v0.3.1 ← via rand 0.8
rand_chacha v0.9.0 ← via rand 0.9
rand_core v0.6.4 ← via rand 0.8
rand_core v0.9.5 ← via rand 0.9
rustix v0.38.44  ← via tempfile
rustix v1.1.4    ← via tempfile / fastrand
shlex v1.3.0     ← firestream-nix-build (direct)
shlex v2.0.1     ← via clap_builder
thiserror v1.0.69 ← workspace pin
thiserror v2.0.18 ← via opentelemetry
webpki-roots v0.26.11 ← via reqwest / rustls
webpki-roots v1.0.7   ← via aws-lc-rs / tonic
libc v0.2.186 (×2) ← appears in both normal and build-dep graphs;
                     same version, not a real duplicate
```

### Why not resolve via `[patch.crates-io]`?

Each of these duplicates would require patching either the upstream
crate that pulls the older version OR forking it. For example:

- Forcing `axum` 0.8 across the tree would require `otel-cli` to migrate
  off axum 0.7's API surface — a real code change, out of scope for Phase 1.
- Forcing `rand` 0.9 across the tree would require `otel-cli` to update its
  direct `rand = "0.8"` dep — possible, but also breaks the
  `cargo update`-style upgrade path. Better deferred.
- `thiserror` 2 cannot be forced down to 1 because `opentelemetry` 0.32
  requires 2; conversely forcing thiserror 2 in the workspace would
  invalidate all current `thiserror = "1"` derives in our code.

These duplicates are stable upstream-pinning artifacts and cost only build
time + binary size, not correctness. Phase 2+ may revisit specific ones
(e.g., bump `otel-cli` to `axum 0.8`) but they are deliberately not
addressed in Phase 1, whose scope is the workspace restructure itself.

### Post-firestream-ci-merge additions

The `firestream-ci` crate added two new transitive duplicates beyond the list
above. Both are accepted under the same rationale (upstream pins, no
correctness impact):

```
crossterm v0.29.0 ← via comfy-table (firestream-ci direct)
                    [crossterm v0.28.1 already present via ratatui]
either v1.16.0    ← appears in two separate transitive trees:
                    via itertools 0.13/0.14 (otel-cli) and via
                    which 6.0 (firestream-ci direct)
```

## Phase 2: Python `nix-fast-build` retirement (2026-06)

Cutover from the vendored Python `nix-fast-build` fork to the Rust port
`firestream-nix-build` (consumed in-process by `firestream-ci` via
`firestream_nix_build::run::run`) is complete:

- `src/lib/nix/vendor/nix-fast-build/` deleted.
- `flake.nix` `nix-fast-build` overlay package and `packages.<system>.nix-fast-build` output removed.
- `src/lib/nix/modules/core.nix` no longer adds `nix-fast-build` to the devShell.
- `bin/_lib.sh` dev-env sentinel probes `firestream-ci` (the CI entrypoint) instead of `nix-fast-build`.
- Stale `nix-fast-build` references in `bin/ci/check.sh` and `bin/build/_common.sh` comments updated.

`firestream-nix-build` is now the sole nix-fast-build implementation in the
tree. Filename prefixes (`nix-fast-build-{phase}.{attr}.{log,json}` in
`firestream-ci/src/util/log_paths.rs`) are retained as a stable on-disk
convention.
