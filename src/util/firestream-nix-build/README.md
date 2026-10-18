# firestream-nix-build

Rust port of [`nix-fast-build`](https://github.com/Mic92/nix-fast-build) with
in-process OpenTelemetry ingest.

This crate is the **sole** `nix-fast-build` implementation in the tree. The
vendored Python fork it was ported from (`src/lib/nix/vendor/nix-fast-build/`)
is deleted; `firestream-ci` consumes this crate in-process via
`firestream_nix_build::run::run`. See `docs/util-migration-notes.md` at the
repo root for the cutover record.

See `FORK.md` for lineage and upstream attribution.

## Build

```bash
cargo build --release --manifest-path src/util/firestream-nix-build/Cargo.toml
nix build .#firestream-nix-build
```

The crate is a member of the `src/util/` virtual Cargo workspace, alongside
`firestream-ci` and `firestream-otel-cli`. That workspace is separate from the
monorepo root's — see `src/util/README.md` for why — so it does not share the
root `target/`.

## Key flags

Same CLI surface as the Python tool it replaced. The fork-specific OTel flags
are preserved bit-for-bit:

| Flag | Meaning |
|---|---|
| `--otel-ingest` | Emit per-derivation OTel spans. |
| `--otel-parent-trace` | W3C traceparent for span nesting (env: `TRACEPARENT`). |
| `--otel-service` | service.name (env: `OTEL_SERVICE_NAME`, default `firestream-ci`). |
| `--otel-cli` | Accepted for CLI compat; **ignored** — ingest runs in-process. |
