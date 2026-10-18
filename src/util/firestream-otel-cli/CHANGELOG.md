# Changelog

## [0.1.0] - 2026-05-27

Initial Rust port of [equinix-labs/otel-cli](https://github.com/equinix-labs/otel-cli).
Phases 1-10 complete.

### Added
- Full CLI parity with the Go upstream: `span` (+ `background`/`event`/`end`),
  `exec`, `status`, `server`, `version`, `completion`
- Transports: gRPC (tonic), HTTP/protobuf, HTTP/JSON, JSON-to-file directory
  writer, null
- W3C traceparent parse/encode + env/file IO
- Embedded OTLP receiver (gRPC + HTTP) for `server` mode; ratatui TUI;
  JSON-directory sink
- Span-background unix-socket IPC (length-prefixed JSON frames; Unix only)
- 113 tests (96 unit + 17 integration); `clippy --all-targets -- -D warnings`
  clean

### Differences from the Go upstream
- `http/json` transport (new)
- `json+file` transport (new) — writes spans directly to a
  `<dir>/<traceHex>/<spanHex>/span.json` tree, byte-equivalent to `server json`
  output
- TLS cert-file loading deferred (only `--tls-no-verify` is honored)
- Windows: `span background` not supported (Unix only)
