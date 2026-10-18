//! Embedded OTLP receiver (used by `otel-cli server`).
//!
//! Go reference: `otlpserver/`. Phase 7 implements:
//! - common `SpanSink` (json-to-disk / tui channel / test mpsc)
//! - tonic-based gRPC receiver (`grpc::run`)
//! - axum-based HTTP receiver (`http::run`)
//! - JSON-to-disk sink (`json::JsonSink`) — built on `crate::json_layout`
//! - ratatui TUI renderer (`tui::run_tui`)
//!
//! The top-level helper `run_server` spawns whichever transports are enabled,
//! shares a single `Arc<SpanSink>`, and shuts both down on max-spans or
//! Ctrl-C.

pub mod grpc;
pub mod http;
pub mod json;
pub mod tui;

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use anyhow::Result;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use tokio::sync::{mpsc, oneshot, Notify};

use self::json::JsonSink;

/// Where a receiver should push every batch of `ResourceSpans` it accepts.
pub enum SpanSink {
    /// Discard everything (useful as a placeholder).
    #[allow(dead_code)]
    Null,
    /// Write to `<dir>/<traceHex>/<spanHex>/...`.
    Json(JsonSink),
    /// Forward to the TUI renderer task.
    Tui(mpsc::Sender<ResourceSpans>),
    /// Test-only sink: drop into an mpsc the test can drain.
    Channel(mpsc::Sender<ResourceSpans>),
}

impl SpanSink {
    /// Push spans into whichever backend we wrap.
    pub async fn ingest(&self, spans: Vec<ResourceSpans>) -> Result<()> {
        match self {
            SpanSink::Null => Ok(()),
            SpanSink::Json(j) => {
                for rs in &spans {
                    j.write(rs).await?;
                }
                Ok(())
            }
            SpanSink::Tui(tx) | SpanSink::Channel(tx) => {
                for rs in spans {
                    // Best-effort: if the receiver has gone away we just drop.
                    let _ = tx.send(rs).await;
                }
                Ok(())
            }
        }
    }
}

/// Atomic counter shared between receivers so they can cooperate on
/// `--max-spans`.
#[derive(Debug, Default)]
pub struct SpanCounter {
    seen: AtomicU64,
    max: u64, // 0 = unlimited
    done: Notify,
}

impl SpanCounter {
    pub fn new(max: u32) -> Arc<Self> {
        Arc::new(Self {
            seen: AtomicU64::new(0),
            max: max as u64,
            done: Notify::new(),
        })
    }

    /// Increment the counter by `n` (number of *spans*, not batches).
    /// Returns true if we've hit the limit after this call.
    pub fn add(&self, n: u64) -> bool {
        if self.max == 0 || n == 0 {
            return false;
        }
        let prev = self.seen.fetch_add(n, Ordering::SeqCst);
        if prev + n >= self.max {
            self.done.notify_waiters();
            true
        } else {
            false
        }
    }

    /// Block until the counter signals "done".
    pub async fn wait(&self) {
        if self.max == 0 {
            // unlimited — never returns
            std::future::pending::<()>().await;
            return;
        }
        // Re-check immediately in case we already crossed the threshold.
        if self.seen.load(Ordering::SeqCst) >= self.max {
            return;
        }
        self.done.notified().await;
    }
}

/// Count the total number of `Span`s flattened from a `Vec<ResourceSpans>`.
pub fn count_spans(rs: &[ResourceSpans]) -> u64 {
    let mut n: u64 = 0;
    for r in rs {
        for ss in &r.scope_spans {
            n += ss.spans.len() as u64;
        }
    }
    n
}

/// Spawn the gRPC and/or HTTP receivers and block until one of:
/// - `max_spans` accepted spans
/// - Ctrl-C
pub async fn run_server(
    grpc_addr: Option<SocketAddr>,
    http_addr: Option<SocketAddr>,
    max_spans: u32,
    sink: Arc<SpanSink>,
) -> Result<()> {
    let counter = SpanCounter::new(max_spans);

    let (grpc_shutdown_tx, grpc_shutdown_rx) = oneshot::channel::<()>();
    let (http_shutdown_tx, http_shutdown_rx) = oneshot::channel::<()>();

    let grpc_handle = grpc_addr.map(|addr| {
        let s = sink.clone();
        let c = counter.clone();
        tokio::spawn(async move { grpc::run(addr, s, c, grpc_shutdown_rx).await })
    });

    let http_handle = http_addr.map(|addr| {
        let s = sink.clone();
        let c = counter.clone();
        tokio::spawn(async move { http::run(addr, s, c, http_shutdown_rx).await })
    });

    // Wait for max-spans, ctrl-c, or both receivers to fail.
    tokio::select! {
        _ = counter.wait() => {
            tracing::info!("server: max-spans reached, shutting down");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("server: ctrl-c received, shutting down");
        }
    }

    // Signal shutdown. If a transport was never spawned, dropping its
    // matching tx here is a no-op; if it *was* spawned it will exit cleanly.
    let _ = grpc_shutdown_tx.send(());
    let _ = http_shutdown_tx.send(());

    if let Some(h) = grpc_handle {
        let _ = h.await;
    }
    if let Some(h) = http_handle {
        let _ = h.await;
    }

    Ok(())
}
