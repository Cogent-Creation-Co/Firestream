# otel-cli (Rust)

[![stability](https://img.shields.io/badge/stability-experimental-lightgrey.svg)](https://github.com/packethost/standards/blob/master/experimental-statement.md)

Rust port of [equinix-labs/otel-cli](https://github.com/equinix-labs/otel-cli) — an
OpenTelemetry CLI for emitting spans from shell scripts and CI pipelines.

## Why

The Go original is excellent. This Rust port exists to:

- ship as a **single statically-linkable binary** alongside other Rust tooling
  in this monorepo (no Go toolchain in the build pipeline);
- match the upstream UX command-for-command so existing scripts keep working;
- add two extensions the Go upstream does not have:
  - **`http/json`** OTLP transport (gRPC + HTTP/protobuf are also supported);
  - **`json+file`** — write spans straight to a directory tree with no collector
    in the loop. Byte-equivalent to `server json` output, useful for tests and
    offline workflows.

## Status

Experimental. Reference is the Go upstream at commit
[`8f86e487`](https://github.com/equinix-labs/otel-cli/commit/8f86e487b5f5badc8a4de50f0108e04bb38a7b4d).
Feature-parity with two known gaps (see [Differences](#differences-from-the-go-upstream)):

- TLS cert-file loading is deferred (only `--tls-no-verify` is honored).
- `span background` is Unix-only (no Windows named-pipe support).

113 tests pass; `clippy --all-targets -- -D warnings` is clean.

## Build

```shell
cd src/lib/rust/otel-cli/otel-cli-rs
cargo build --release
./target/release/otel-cli --help
```

A multi-stage `Dockerfile` is included; see [Docker](#docker) below.

## Quickstart

### Run a local collector in one terminal

```shell
otel-cli server --grpc-addr 127.0.0.1:4317 --http-addr '' --max-spans 5 json --dir /tmp/spans
```

This embeds an OTLP receiver and writes each span to
`/tmp/spans/<traceHex>/<spanHex>/span.json`. Use `server tui` instead for an
interactive ratatui table.

### Send a span

```shell
otel-cli span \
  --endpoint http://127.0.0.1:4317 \
  --protocol grpc \
  --service my-service \
  --name my-task
```

### Wrap a command

```shell
otel-cli exec --service my-service --name "curl google" -- curl https://google.com
```

The wrapped process's stdout, stderr, and exit code are passed through verbatim;
the resulting span records duration and exit status.

### Chain spans via traceparent

```shell
otel-cli exec --kind producer -- otel-cli exec --kind consumer sleep 1
```

`otel-cli` injects `TRACEPARENT` into the child's environment, so the inner
`exec` automatically becomes a child span of the outer. `{{traceparent}}` token
substitution in argv is also supported:

```shell
otel-cli exec --name "curl api" -- \
   curl -H 'traceparent: {{traceparent}}' https://myapi.com/v1/coolstuff
```

### Background span (full lifecycle)

```shell
sockdir=$(mktemp -d)
otel-cli span background \
    --service "$0" \
    --name "$0 runtime" \
    --sockdir "$sockdir" &        # background server blocks; & is required
sleep 0.1                         # give the listener a moment to bind
otel-cli span event --name "cool thing" --attrs "foo=bar" --sockdir "$sockdir"
otel-cli span end --sockdir "$sockdir"
# or: `kill %1` cleanly ends the span on SIGTERM
```

Unix only — uses an `AF_UNIX` stream socket with a length-prefixed JSON wire
protocol.

### Standalone JSON-to-disk (no collector)

```shell
otel-cli span --protocol json+file --json-dir /tmp/spans --service t --name s
```

`json+file` is byte-equivalent to what `server json` would write. Great for
tests, CI artifacts, or feeding spans into other tooling.

### Custom timestamps

```shell
# RFC3339 with or without nanos
otel-cli span --start 2021-03-24T07:28:05.12345Z --end 2021-03-24T07:30:08.0001Z
# Unix epoch, optional decimal nanos
otel-cli span --start 1616620946 --end 1616620950.241980634

start=$(date --rfc-3339=ns)
some-interesting-program --with-some-options
end=$(date +%s.%N)
otel-cli span -n my-script -s some-interesting-program --start "$start" --end "$end"
```

## Command reference

| Command          | Purpose                                                                    |
| ---------------- | -------------------------------------------------------------------------- |
| `span`           | Create and send a single span. Run `otel-cli span --help`.                 |
| `span background`| Spawn a Unix-socket server that holds a span open while you add events.    |
| `span event`     | Attach an event to a running background span (`--sockdir`).                |
| `span end`       | End a running background span (`--sockdir`).                               |
| `exec`           | Wrap a child process in a span; injects `TRACEPARENT` into its env.        |
| `status`         | Emit canary spans and print the resolved config + diagnostics as JSON.     |
| `server json`    | Embedded OTLP receiver that writes each received span as JSON to a dir.    |
| `server tui`     | Embedded OTLP receiver with a live ratatui table of received spans.        |
| `completion`     | Generate shell completion scripts: `bash`, `zsh`, `fish`, `powershell`.    |
| `version`        | Print version.                                                             |

Append `--help` to any command for full flag documentation.

## Environment variables

`otel-cli` honors the standard OTel environment variables plus the
`OTEL_CLI_*` variables from the Go upstream. Precedence at runtime is
**CLI flags > env > JSON config > defaults**.

### OTLP transport

| Variable                                  | Effect                                      |
| ----------------------------------------- | ------------------------------------------- |
| `OTEL_EXPORTER_OTLP_ENDPOINT`             | Default endpoint for all signals.           |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`      | Override just for traces.                   |
| `OTEL_EXPORTER_OTLP_PROTOCOL`             | `grpc` \| `http/protobuf` \| `http/json` \| `json+file`. |
| `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`      | Override just for traces.                   |
| `OTEL_EXPORTER_OTLP_TIMEOUT`              | Duration string (`1s`, `500ms`).            |
| `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`       | Override just for traces.                   |
| `OTEL_EXPORTER_OTLP_HEADERS`              | `k1=v1,k2=v2` header list.                  |
| `OTEL_EXPORTER_OTLP_INSECURE`             | Bool — allow plaintext on ambiguous URLs.   |
| `OTEL_EXPORTER_OTLP_BLOCKING`             | Bool — block until transport is connected.  |

### TLS

| Variable                                       | Effect                                     |
| ---------------------------------------------- | ------------------------------------------ |
| `OTEL_EXPORTER_OTLP_CERTIFICATE`               | CA cert path (deferred — see Differences). |
| `OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE`        | Per-traces CA cert path.                   |
| `OTEL_EXPORTER_OTLP_CLIENT_KEY`                | Client key path.                           |
| `OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY`         | Per-traces client key path.                |
| `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`        | Client cert path.                          |
| `OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE` | Per-traces client cert path.               |
| `OTEL_CLI_TLS_NO_VERIFY`                       | Bool — skip TLS verification.              |
| `OTEL_CLI_NO_TLS_VERIFY`                       | Alias of the above (Go-upstream typo).     |

### Span content

| Variable                            | Effect                                        |
| ----------------------------------- | --------------------------------------------- |
| `OTEL_SERVICE_NAME`                 | Standard OTel service name.                   |
| `OTEL_CLI_SERVICE_NAME`             | otel-cli alias of the above.                  |
| `OTEL_CLI_SPAN_NAME`                | Default span name.                            |
| `OTEL_CLI_TRACE_KIND`               | `internal`\|`server`\|`client`\|`producer`\|`consumer`. |
| `OTEL_CLI_ATTRIBUTES`               | `k=v,k=v` attributes.                         |
| `OTEL_CLI_STATUS_CODE`              | `unset` \| `ok` \| `error`.                   |
| `OTEL_CLI_STATUS_DESCRIPTION`       | Free text.                                    |
| `OTEL_CLI_FORCE_TRACE_ID`           | 32 hex chars — force a trace id.              |
| `OTEL_CLI_FORCE_SPAN_ID`            | 16 hex chars — force a span id.               |
| `OTEL_CLI_FORCE_PARENT_SPAN_ID`     | 16 hex chars — force a parent span id.        |

### Traceparent (W3C)

| Variable                          | Effect                                                |
| --------------------------------- | ----------------------------------------------------- |
| `TRACEPARENT`                     | Standard W3C traceparent input.                       |
| `OTEL_CLI_CARRIER_FILE`           | Carrier file for traceparent IO.                      |
| `OTEL_CLI_IGNORE_ENV`             | Bool — ignore `TRACEPARENT` even if set.              |
| `OTEL_CLI_PRINT_TRACEPARENT`      | Bool — print resulting traceparent to stdout.         |
| `OTEL_CLI_EXPORT_TRACEPARENT`     | Bool — print as `export TRACEPARENT=...`.             |
| `OTEL_CLI_TRACEPARENT_REQUIRED`   | Bool — error out if no valid traceparent is loaded.   |

### exec / misc

| Variable                          | Effect                                          |
| --------------------------------- | ----------------------------------------------- |
| `OTEL_CLI_EXEC_CMD_TIMEOUT`       | Timeout for the wrapped child (duration).       |
| `OTEL_CLI_EXEC_TP_DISABLE_INJECT` | Bool — don't inject `TRACEPARENT` into child.   |
| `OTEL_CLI_CONFIG_FILE`            | Path to a JSON config file.                     |
| `OTEL_CLI_VERBOSE`                | Bool — verbose logging.                         |
| `OTEL_CLI_FAIL`                   | Bool — exit non-zero on transport errors.       |

JSON config files use the snake_case form of each field — see
`src/config.rs::Config` for the exact schema.

## Differences from the Go upstream

**New extensions:**

- **`http/json`** protocol — OTLP/HTTP with `application/json` body. Not in the
  Go upstream because `opentelemetry-go` doesn't support it on the client side.
- **`json+file`** protocol — write directly to a `<dir>/<traceHex>/<spanHex>/span.json`
  tree. Byte-equivalent to what `server json` writes. Lets you generate spans in
  fully-offline test runs with no collector process.

**Deferred features (not yet implemented):**

- TLS client/CA cert file loading. The flags and env vars are parsed and stored,
  but only `--tls-no-verify` actually affects the transport today. Use a
  collector or a CA-trust-store sidecar in the meantime.
- `span background` on Windows. The Go upstream supports named pipes; the Rust
  port currently only implements the `AF_UNIX` path.
- A full pterm-equivalent TUI. `server tui` uses ratatui and shows a live span
  table, but lacks the rich detail panes of the Go version.

## Docker

```shell
docker build -t otel-cli:dev .
docker run --rm otel-cli:dev version
docker run --rm otel-cli:dev status
```

The image is `debian:bookworm-slim` plus the binary plus `ca-certificates`.

## Testing

```shell
cargo test                         # 113 tests (96 unit + 17 integration)
cargo clippy --all-targets -- -D warnings
bash tests/integration.sh          # end-to-end smoke test, no collector needed
```

## License

Apache-2.0. See the workspace `LICENSE` file.
