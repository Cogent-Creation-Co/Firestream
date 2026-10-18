# FORK.md — `firestream-otel-cli`

This crate is a **Rust port of a Go project**. This document is the provenance
and statement-of-changes record required by the Apache License, Version 2.0,
§4(b) and §4(d). It is referenced by `NOTICE` and forms part of it.

## Lineage

| Field | Value |
|-------|-------|
| **Upstream project** | `otel-cli` |
| **Upstream URL** | https://github.com/equinix-labs/otel-cli |
| **Upstream language** | **Go** |
| **Upstream license** | Apache-2.0 |
| **Reference commit** | `8f86e487b5f5badc8a4de50f0108e04bb38a7b4d` |
| **This crate's license** | Apache-2.0 (`./LICENSE`, `./NOTICE`) |
| **Cargo package** | `firestream-otel-cli` |
| **Library name** | `otel_cli` *(deliberately unchanged)* |
| **Binary name** | `otel-cli` *(deliberately unchanged)* |

> **Verification note.** The reference commit hash above is **carried forward
> from this crate's own `README.md`** (see its "Status" section, which links
> `8f86e487`). It was not re-verified against the upstream repository — the
> port was lifted into Firestream on a host with no network access, and no
> copy of the upstream Go tree exists in this repository. Treat it as the
> port author's claim, not as an independently confirmed fact.

The package name is `firestream-otel-cli` to namespace it within the Firestream
tree, but the **lib target stays `otel_cli` and the bin target stays
`otel-cli`**. Matching upstream command-for-command is the entire point of the
port: scripts written against the Go original must keep working unchanged.
Renaming the binary would destroy that property.

## Why a Rust rewrite was necessary

This is the substantive justification, not a formality. Three reasons, in
increasing order of decisiveness.

### 1. Toolchain closure

Firestream's build pipeline is **Nix + Rust end to end**. Every builder image
and every `nix develop` closure is derived from that toolchain. Adopting the Go
original would mean adding a full Go toolchain to each of them — to every
builder image and every developer shell — for the sole purpose of emitting
OpenTelemetry spans. That is a large, permanent cost on the critical path of
every build, paid for a telemetry side-concern.

### 2. One statically-linkable binary among peers

The port builds as a single statically-linkable binary that sits alongside the
rest of the Rust tooling (`firestream-ci`, `firestream-nix-build`) in the same
Cargo workspace, sharing one dependency graph, one lockfile, one `cargo build`,
and one release profile (`src/util/Cargo.toml`). A Go binary would be a
separate build system producing a separate artifact with separate caching.

### 3. Decisively: it is consumed as a *library*, not a subprocess

This is the reason a rewrite was **necessary** rather than merely convenient. A
Go binary cannot be linked into a Rust program; it can only be spawned. This
port is linked.

**Verified in code** (paths relative to `src/util/`):

- `firestream-ci/Cargo.toml:32-33` — `firestream-ci` takes both
  `firestream-otel-cli` and `firestream-nix-build` as ordinary path
  dependencies.
- `firestream-ci/src/nix/mod.rs:109` — calls
  `firestream_nix_build::run::run(opts).await` **in-process**. The Nix build
  driver is a linked function call, not a spawned CLI.
- `firestream-ci/src/nix/mod.rs:308` — `reconcile()` calls
  `otel_cli::reconcile_spans(result_file, spans_dir, spans_dir).await`
  **directly**. Span reconciliation is a linked function call.
- `firestream-ci/src/nix/mod.rs:11` — the module's own doc comment states this:
  *"[`otel_cli::reconcile_spans`] directly (no subprocess)."*
- `firestream-nix-build/src/otel.rs:13` — imports
  `otel_cli::cli::nix_ingest::InProcessIngest`; the per-derivation span ingest
  state machine runs inside the same process as the build driver.
- `firestream-nix-build/src/display.rs:19` — imports
  `otel_cli::ingest::nix_internal_json::{…}` to parse `nix --log-format
  internal-json` output in-process.

**Consequence: a full CI run spawns zero span-emitter subprocesses.** This was
checked rather than assumed. The one apparent counter-example is not one:

- `firestream-ci/src/nix/mod.rs:179` sets `otel_cli_bin: "otel-cli".into()`, and
  `firestream-nix-build/src/cli.rs:197` exposes an `--otel-cli` flag defaulting
  to `otel-cli`. Both exist purely for CLI compatibility with the Python
  `nix-fast-build` fork's flag surface.
- Grepping `otel_cli_bin` across `firestream-nix-build/src` yields exactly two
  hits — `src/options.rs:100` (the field declaration) and `src/cli.rs:404` (the
  assignment). **The value is stored and never executed.** There is no spawn
  site.

By contrast, the Python `nix-fast-build` this toolkit's sibling crate replaces
spawned one `otel-cli nix-ingest` subprocess *per `nix build`* (see
`../firestream-nix-build/FORK.md`). Linking removes that entire process-per-build
cost and the associated span-attribution fragility.

## Extensions over upstream

Two transports exist here that the Go upstream does not provide. **Both were
verified to exist in this tree before being claimed.**

### `http/json` OTLP transport

OTLP over HTTP with an `application/json` request body.

- Implementation: `src/client/http_json.rs`.
- Protocol dispatch: `src/config.rs:644` — `"http/json" => return Protocol::HttpJson`.
- Documented in `src/cli/common.rs:20` alongside the other protocol values.

Absent upstream because `opentelemetry-go` does not support OTLP/HTTP+JSON on
the client side. It is useful against collectors and gateways where a JSON body
is inspectable and debuggable in a way a protobuf body is not.

### `json+file` sink

Writes spans **straight to a directory tree** — `<dir>/<traceHex>/<spanHex>/span.json`
— with **no collector process anywhere in the loop**.

- Implementation: `src/client/json_file.rs`.
- Shared rendering with the embedded receiver: `src/json_layout.rs` (its header
  notes the Go reference `otelcli/server_json.go::renderJson`, and that both the
  `json+file` client and `server json` produce the same layout — i.e. output is
  byte-equivalent between the two).
- Recording predicate and dir handling: `src/config.rs:619`, `:632`, `:189`.
- Reused by the embedded server's crash-recovery replay path, which re-emits
  orphaned checkpoint spans into the same sink: `src/cli/server.rs:105-121`.

**Why this one matters.** It is the property that makes a Firestream build
**observable on a laptop with nothing else installed**. Every other OTLP
transport requires something to receive the spans: a collector, a gateway, a
SaaS endpoint, credentials. `json+file` requires a directory. A developer runs
a build and gets a complete, structured span tree on disk with zero
infrastructure, zero configuration, and zero network. That is also what makes
spans usable as ordinary CI artifacts, and what lets the test suite assert on
span output in fully offline runs.

## Divergences and parity gaps

Carried over faithfully from the "Differences from the Go upstream" section of
`./README.md`. Two of these are also called out in that file's "Status"
section as the known parity gaps.

**Deferred features (not yet implemented):**

- **TLS client/CA certificate-file loading.** The flags and environment
  variables are parsed and stored, but only `--tls-no-verify` actually affects
  the transport today. Use a collector or a CA-trust-store sidecar in the
  meantime. Affected env vars: `OTEL_EXPORTER_OTLP_CERTIFICATE`,
  `OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_KEY`,
  `OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY`,
  `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`,
  `OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE`.
- **`span background` on Windows.** The Go upstream supports named pipes; this
  port implements only the `AF_UNIX` path, so `span background` is Unix-only.
- **A full pterm-equivalent TUI.** `server tui` uses `ratatui` and shows a live
  span table, but lacks the rich detail panes of the Go version.

**Retained from upstream deliberately:**

- The full upstream command surface: `span`, `span background`, `span event`,
  `span end`, `exec`, `status`, `server json`, `server tui`, `completion`,
  `version`.
- The standard OTel environment variables plus the `OTEL_CLI_*` variables from
  the Go upstream, including the upstream's own typo alias
  `OTEL_CLI_NO_TLS_VERIFY` for `OTEL_CLI_TLS_NO_VERIFY`.
- Upstream precedence semantics: **CLI flags > env > JSON config > defaults**.

**Stability.** The crate's `README.md` describes the port as *experimental* and
reports 113 passing tests with a clean `clippy --all-targets -- -D warnings`.
Those figures are carried forward from that README and were not re-run as part
of writing this document.

## Licensing summary

- Upstream `equinix-labs/otel-cli` is **Apache-2.0**. Its license text is
  retained verbatim at `./LICENSE`.
- Attribution and the statement of changes required by Apache-2.0 §4(b) and
  §4(d) are in `./NOTICE`, which incorporates this file by reference.
- Modifications in this port are Apache-2.0 as well, per the `src/util/`
  subtree license (`../LICENSE`).
- **The Firestream root `LICENSE` (MIT) does not cover `src/util/`.** See
  `../README.md`.
