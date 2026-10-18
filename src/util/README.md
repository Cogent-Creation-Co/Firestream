# `src/util/` — the Firestream CI toolkit

Three Rust crates that together form Firestream's build/CI tooling. They live in
their **own Cargo workspace**, separate from the Firestream root workspace, and
under a **different license** from the rest of the repository.

---

## ⚠️ Licensing — read this first

**The Firestream root `LICENSE` (MIT) does NOT cover `src/util/`.**

This subtree is licensed **Apache-2.0**. Its license text is `src/util/LICENSE`.

| Path | License | Notice files |
|---|---|---|
| Firestream repository root | MIT | `/LICENSE` |
| **`src/util/` (this subtree)** | **Apache-2.0** | **`src/util/LICENSE`** |
| `src/util/firestream-ci/` | Apache-2.0 (original work, no upstream) | — |
| `src/util/firestream-otel-cli/` | Apache-2.0 — port of an Apache-2.0 project | `LICENSE`, `NOTICE`, `FORK.md` |
| `src/util/firestream-nix-build/` | `Apache-2.0 AND MIT` — port of an MIT project | `LICENSE-MIT`, `FORK.md` |

Two of the three crates are **ports of upstream projects** and carry obligations
beyond the subtree license:

- **`firestream-otel-cli`** is a Rust port of the Go project
  [`equinix-labs/otel-cli`](https://github.com/equinix-labs/otel-cli), which is
  Apache-2.0. Apache-2.0 §4(b) and §4(d) require retaining the license and the
  attribution notices, and **stating the changes made**. That is what its
  `LICENSE`, `NOTICE`, and `FORK.md` are for. All three must ship with any
  redistribution.
- **`firestream-nix-build`** is a Rust port of the Python project
  [`Mic92/nix-fast-build`](https://github.com/Mic92/nix-fast-build), which is
  **MIT**. MIT requires retaining the upstream copyright and permission notice,
  which is why `LICENSE-MIT` exists here in addition to the subtree's
  Apache-2.0 `LICENSE`. Its `Cargo.toml` declares the combined work honestly as
  `Apache-2.0 AND MIT`.

If you vendor, redistribute, or relicense any part of this subtree, start from
the per-crate `FORK.md` files — they are the authoritative provenance records,
and each flags which of its claims are independently verified versus carried
forward from the porting project's own documentation.

---

## The three crates

| Crate | Package | Lib / bin | What it is |
|---|---|---|---|
| `firestream-ci` | `firestream-ci` | `firestream_ci` / `firestream-ci` | The CI orchestrator. Original work — **no upstream**. Typed replacement for build/CI shell: worktree resolution, run directories, manifests, artifact materialisation, OCI image handling, platform/strategy detection, and the phase pipeline. |
| `firestream-otel-cli` | `firestream-otel-cli` | `otel_cli` / `otel-cli` | OpenTelemetry CLI **and library** for emitting spans from builds and pipelines. Rust port of the Go `equinix-labs/otel-cli`, plus two transports upstream lacks: `http/json` and `json+file`. |
| `firestream-nix-build` | `firestream-nix-build` | `firestream_nix_build` / `firestream-nix-build` | Parallel, per-attribute `nix build` driver with a bounded job queue and failure isolation. Rust port of the Python `Mic92/nix-fast-build`, with in-process OTel ingest. |

The lib and bin target names for `firestream-otel-cli` are **deliberately left
as upstream's** (`otel_cli` / `otel-cli`) — matching the Go original
command-for-command is the point of that port, and renaming the binary would
break scripts written against it.

The three compose by **linking**, not by spawning: `firestream-ci` depends on
both siblings as ordinary path dependencies and calls
`firestream_nix_build::run::run` and `otel_cli::reconcile_spans` in-process. A
full CI run spawns zero span-emitter subprocesses. This is the property that
made a Rust rewrite of the Go `otel-cli` necessary rather than merely
convenient — a Go binary can only be spawned, never linked. See
`firestream-otel-cli/FORK.md`.

---

## Why an isolated Cargo workspace

`src/util/` is a virtual workspace with its own `Cargo.toml`, `Cargo.lock`, and
`target/`. It is **not** a member of the Firestream root workspace. The reasons:

1. **Edition and MSRV differ.** Firestream's crates are `edition = "2024"`.
   This subtree is `edition = "2021"` with `rust-version = "1.82"`.
2. **Dependency-graph conflict.** This subtree's lockfile carries 15+ documented
   duplicate transitive major versions across `tonic`, `axum` (×2), `reqwest`,
   `rustls`, `bollard`, `git2`, `superconsole`, and `ratatui` — see
   `docs/util-migration-notes.md`. Merging that into the root lockfile would slow every
   root `cargo build --workspace` and fight Firestream's own version pins.
3. **Reusability by construction.** The toolkit is meant to stay
   project-agnostic: everything Firestream-specific is expressed as a JSON data
   payload rather than compiled-in code. Physical separation from the
   application workspace is part of how that invariant is kept honest.

**Accepted consequence:** the root `cargo test --workspace` and
`cargo build --workspace` do **not** cover `src/util/`. This subtree gets its
own make targets and its own flake package.

```bash
# Build and test this subtree (from the repository root)
cd src/util && cargo build --workspace
cd src/util && cargo test  --workspace
```

---

## Files in this directory

| File | Purpose |
|---|---|
| `LICENSE` | Apache-2.0 text governing this subtree. |
| `README.md` | This file. |
| `Cargo.toml` | Virtual workspace root; `[workspace.package]` and shared dependency pins. |
| `Cargo.lock` | This subtree's lockfile, independent of the root one. |
| `deny.toml` | `cargo-deny` configuration for this workspace. |
