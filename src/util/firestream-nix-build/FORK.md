# FORK.md — `firestream-nix-build`

This crate is a **Rust port of a Python project**. This document records its
provenance, its divergences from upstream, and its licensing.

## Lineage

| Field | Value |
|-------|-------|
| **Upstream project** | `nix-fast-build` |
| **Upstream URL** | https://github.com/Mic92/nix-fast-build |
| **Upstream language** | **Python** |
| **Upstream license** | **MIT** (retained at `./LICENSE-MIT`) |
| **Reference commit** | `7f185e0ec37b65b4730f892e0de9a831b0610f3a` (v1.5.0) |
| **Port date** | 2026-05-29 |
| **Origin repository of this port** | ConceptDB, as `src/util/oxi-nix-fast-build` |
| **Cargo package** | `firestream-nix-build` |
| **Library name** | `firestream_nix_build` |
| **Binary name** | `firestream-nix-build` |

> **Verification note.** The reference commit, the version tag, and the port
> date are **carried forward verbatim from the FORK.md this file replaces**,
> which was written in the origin repository (ConceptDB). They were not
> re-verified here: this crate was lifted into Firestream on a host with no
> network access, and no copy of the upstream Python tree exists anywhere in
> this repository. Treat them as the original port author's claims rather than
> as independently confirmed facts.

## What this is

A line-for-line port of the Python `nix-fast-build` tool to Rust, preserving
the upstream CLI surface and behaviour — including the `--otel-ingest` family
of flags that originated in the porting project's Python fork rather than in
Mic92's upstream.

Those flags are present in this tree and were verified:

- `src/cli.rs:181` — `--otel-ingest`
- `src/cli.rs:185` — `--otel-parent-trace` (env `TRACEPARENT`)
- `src/cli.rs:189` — `--otel-service`

## Notable design changes vs. the Python fork

### In-process OTel ingest

The Python fork spawns one `otel-cli nix-ingest` **subprocess per `nix build`**.
This port links the sibling `firestream-otel-cli` crate as a **library** and
runs the ingest state machine in-process, one independent ingest per build, with
a single shared OTLP emit client.

Verified in this tree:

- `Cargo.toml` — `firestream-otel-cli = { path = "../firestream-otel-cli" }`,
  commented *"In-process OTel ingest. Linked statically; no subprocess per
  build."*
- `src/otel.rs:13` — `use otel_cli::cli::nix_ingest::InProcessIngest;`
- `src/otel.rs:73` — `InProcessIngest::new(parent_trace, service, min_level).await`
- `src/otel.rs:18` — doc comment: each build *"spawns its own `InProcessIngest`
  state machine via `PerBuildIngest::fresh_state`"*, which is what keeps
  activity-id maps isolated between concurrent builds.
- `src/display.rs:19` — `otel_cli::ingest::nix_internal_json::{…}`, parsing
  `nix --log-format internal-json` in-process.

> The per-build isolation requirement was documented upstream in the Python
> fork's `__init__.py:126-128` comment about activity-id collisions. That
> citation is **carried forward from the previous FORK.md and is not verifiable
> here** — no Python source is present in this repository. The behaviour it
> describes is nevertheless implemented and verifiable above.

There is a residual `--otel-cli` flag (`src/cli.rs:197`, default `otel-cli`)
accepted for CLI compatibility with the Python fork. **It is inert.** Grepping
`otel_cli_bin` across `src/` yields exactly two hits — `src/options.rs:100`
(field declaration) and `src/cli.rs:404` (assignment). The value is stored and
never executed; there is no spawn site.

### Workspace layout

*(This section previously described ConceptDB's layout and was factually wrong
about Firestream; corrected below.)*

In Firestream this crate is **not** its own isolated workspace. It is a member
of the `src/util/` virtual workspace (`src/util/Cargo.toml`, `members = [
"firestream-otel-cli", "firestream-nix-build", "firestream-ci" ]`) and has no
`[workspace]` stanza of its own. It inherits `edition`, `rust-version`,
`authors`, `repository`, and the release profile from that workspace's
`[workspace.package]`.

`src/util/` is nonetheless isolated **from the Firestream root workspace**: it
is a separate workspace with its own `Cargo.lock` and its own `target/`. It is
not listed in the root `Cargo.toml`'s `members`, and no `exclude` entry is
needed because it carries its own `[workspace]`. The reason for the separation
is that Firestream's crates are `edition = "2024"` while this subtree is
`edition = "2021"` / MSRV 1.82 and carries documented duplicate transitive
major versions (see `docs/util-migration-notes.md` at the repo root); merging
the two lockfiles would
slow every root `cargo build --workspace` and fight Firestream's own pins.

The accepted consequence is that the root `cargo test --workspace` does **not**
cover `src/util/`; it gets its own make targets and flake package.

## Cutover status

*(This section previously read "Ships side-by-side with the Python tool. CI
continues to invoke the Python `nix-fast-build` until a follow-up change flips
it over." That statement described the origin repository's older **bash** CI.
**It is stale and does not describe Firestream.** Corrected below.)*

**On the `firestream-ci` code path, the Rust port is the implementation.** No
Python `nix-fast-build` is invoked, and none is vendored in this repository.

Verified:

- `../firestream-ci/Cargo.toml:33` —
  `firestream-nix-build = { path = "../firestream-nix-build" }`.
- `../firestream-ci/src/nix/mod.rs:109` — `FastBuild::run()` calls
  `firestream_nix_build::run::run(opts).await` **in-process**. The doc comment
  at `:98` states this outright: *"Execute the configured run via
  `firestream_nix_build::run::run`."*

Two caveats, stated so this section does not become stale in turn:

1. `firestream-ci` is not yet wired into Firestream's own build. As of this
   commit the `src/util/` subtree has been lifted and licensed (plan phases 1
   and 2); Nix packaging, the CI profile payload, and the pipeline integration
   are later phases. "The Rust port is the implementation" is a statement about
   the `firestream-ci` code path, not yet about `make build`.
2. Retaining a plain `nix build` fallback path remains desirable while the port
   accumulates production mileage.

The previous FORK.md also referenced a **vendored Python fork** at
`src/lib/nix/vendor/nix-fast-build/`. That path is **ConceptDB provenance,
retained here for the historical record only** — it never existed in
Firestream, and it was not found in the ConceptDB working tree either when this
document was written. Nothing in Firestream reads it.

> Note: this crate's `README.md` previously carried the same stale
> side-by-side claim and ConceptDB-relative references (`../../../prd-v4.md`
> §9.2, a `~/.claude/plans/…` path, and "isolated Cargo workspace (empty
> `[workspace]`)"). It has since been reconciled with the corrections above.

## License

This crate is a **derivative work of an MIT-licensed project**, with
Apache-2.0-licensed modifications. Both notices are retained.

| Component | License | Text |
|---|---|---|
| Upstream `Mic92/nix-fast-build` (Python), from which this is ported | **MIT** | `./LICENSE-MIT` |
| Modifications and Rust implementation in this port | **Apache-2.0** | `../LICENSE` |

Consequences:

- MIT requires that the upstream copyright notice and permission notice be
  **retained in all copies or substantial portions**. `./LICENSE-MIT` is that
  retention, and it must ship with any redistribution of this crate. See the
  provenance note at the bottom of that file: the license body is canonical MIT
  text, but the copyright *line* could not be transcribed verbatim from
  upstream's `LICENSE` (no network, no copy on disk) and is flagged for
  replacement.
- The port's own modifications are Apache-2.0, matching the `src/util/` subtree
  (`../LICENSE`) and the `[workspace.package] license = "Apache-2.0"` in
  `../Cargo.toml`.
- The combined work is therefore `Apache-2.0 AND MIT`, which is what this
  crate's `Cargo.toml` declares. This corrects an inconsistency in the previous
  FORK.md, which stated upstream was MIT while the crate inherited a bare
  `Apache-2.0` from the workspace and pointed at a "ConceptDB root LICENSE"
  that does not exist in this repository.
- **The Firestream root `LICENSE` (MIT) does not cover `src/util/`.** See
  `../README.md`. Note that the root license being MIT is a coincidence of this
  repository and is unrelated to upstream `nix-fast-build`'s MIT license; do
  not conflate them.
