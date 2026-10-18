//! Background-span IPC (unix-socket server + client).
//!
//! Go reference: `otelcli/span_background_server.go`. Phase 8.

pub mod client;
pub mod protocol;
pub mod server;
