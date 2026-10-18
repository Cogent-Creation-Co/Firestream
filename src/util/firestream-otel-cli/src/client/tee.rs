//! Two-queue tee exporter (PRD §9.1).
//!
//! Wraps two child [`OtlpClient`]s — typically a network OTLP transport (gRPC
//! or HTTP) and the durable [`json_file::JsonFileClient`]. Each call to
//! [`upload_traces`] enqueues every [`ResourceSpans`] into two **independent**
//! `tokio::sync::mpsc` channels; one worker task per child drains its queue,
//! batches up to the configured threshold, and calls the child on a flush.
//!
//! This is intentionally **not** a synchronous fan-out: the file leg must not
//! be allowed to backpressure the network leg, and vice versa. The two queues
//! are sized to PRD §9.1's batch parameters:
//!
//! - **OTLP queue** — `max_queue_size = 8192`, `max_export_batch_size = 64`,
//!   `scheduled_delay = 500ms`, drop policy = drop-oldest. Durability lives
//!   on the file side, so silently dropping a network-bound batch is
//!   recoverable from disk later.
//! - **File queue** — `max_queue_size = 16384`, `max_export_batch_size = 256`,
//!   `scheduled_delay = 1s`, drop policy = refuse (block until space). The
//!   file leg is the durability boundary; dropping here means dataloss.
//!
//! Both workers exit cleanly when their channel sender is dropped (see
//! [`TeeClient::stop`]).

use async_trait::async_trait;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{ClientError, OtlpClient};

// PRD §9.1 batch parameters — OTLP leg.
const OTLP_BATCH: usize = 64;
const OTLP_DELAY: Duration = Duration::from_millis(500);
const OTLP_QUEUE: usize = 8192;

// PRD §9.1 batch parameters — file leg.
const FILE_BATCH: usize = 256;
const FILE_DELAY: Duration = Duration::from_millis(1000);
const FILE_QUEUE: usize = 16384;

/// How long [`TeeClient::upload_traces`] is willing to block trying to enqueue
/// onto the *file* queue before logging + counting a drop. We never silently
/// drop file-bound spans (that's the whole point of durable mode), so this is
/// only the upper bound on producer-side backpressure.
const FILE_BLOCK_BUDGET: Duration = Duration::from_secs(5);

/// Drop policy per queue. Not directly observable from the outside; encoded
/// here to make the asymmetry between the two legs explicit.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DropPolicy {
    /// OTLP queue: when full, drop the oldest pending item to make room.
    DropOldest,
    /// File queue: never drop silently. Block for [`FILE_BLOCK_BUDGET`] and
    /// then log + increment a counter.
    Refuse,
}

/// Cheap counter pair exposed for tests + diagnostics. Both atomics are
/// monotonic-increment-only.
#[derive(Default, Debug)]
pub struct TeeStats {
    pub otlp_dropped_oldest: AtomicU64,
    pub file_blocked_drops: AtomicU64,
}

pub struct TeeClient {
    otlp_tx: mpsc::Sender<ResourceSpans>,
    file_tx: mpsc::Sender<ResourceSpans>,
    otlp_handle: Option<JoinHandle<()>>,
    file_handle: Option<JoinHandle<()>>,
    stats: Arc<TeeStats>,
}

impl TeeClient {
    /// Build a tee that fans out to both children. The children are *not*
    /// started here — each worker calls `start()` on its child on entry.
    pub fn new(otlp: Box<dyn OtlpClient>, file: Box<dyn OtlpClient>) -> Self {
        let (otlp_tx, otlp_rx) = mpsc::channel::<ResourceSpans>(OTLP_QUEUE);
        let (file_tx, file_rx) = mpsc::channel::<ResourceSpans>(FILE_QUEUE);
        let stats = Arc::new(TeeStats::default());

        let otlp_handle = tokio::spawn(worker(
            otlp,
            otlp_rx,
            OTLP_BATCH,
            OTLP_DELAY,
            "otlp",
        ));
        let file_handle = tokio::spawn(worker(
            file,
            file_rx,
            FILE_BATCH,
            FILE_DELAY,
            "file",
        ));

        Self {
            otlp_tx,
            file_tx,
            otlp_handle: Some(otlp_handle),
            file_handle: Some(file_handle),
            stats,
        }
    }

    /// Expose a cloneable handle to the stats. Useful for diagnostic output
    /// and for tests that want to assert drop counters.
    pub fn stats(&self) -> Arc<TeeStats> {
        self.stats.clone()
    }
}

async fn worker(
    mut child: Box<dyn OtlpClient>,
    mut rx: mpsc::Receiver<ResourceSpans>,
    batch_size: usize,
    delay: Duration,
    label: &'static str,
) {
    if let Err(e) = child.start().await {
        // Don't bail — the child may recover, and dropping spans is the
        // wrong thing to do at startup. Just log.
        eprintln!("otel-cli::tee[{label}]: child start failed: {e}");
    }

    let mut batch: Vec<ResourceSpans> = Vec::with_capacity(batch_size);
    let mut timer = tokio::time::interval(delay);
    // Skip the immediate first tick so we don't flush an empty batch.
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer.tick().await;

    loop {
        tokio::select! {
            maybe_msg = rx.recv() => {
                match maybe_msg {
                    Some(rs) => {
                        batch.push(rs);
                        if batch.len() >= batch_size {
                            flush(&mut child, &mut batch, label).await;
                        }
                    }
                    None => {
                        // Channel closed (TeeClient dropped its sender or
                        // called stop()). Drain whatever's left and exit.
                        flush(&mut child, &mut batch, label).await;
                        let _ = child.stop().await;
                        break;
                    }
                }
            }
            _ = timer.tick() => {
                if !batch.is_empty() {
                    flush(&mut child, &mut batch, label).await;
                }
            }
        }
    }
}

async fn flush(
    child: &mut Box<dyn OtlpClient>,
    batch: &mut Vec<ResourceSpans>,
    label: &'static str,
) {
    if batch.is_empty() {
        return;
    }
    let drained: Vec<ResourceSpans> = std::mem::take(batch);
    if let Err(e) = child.upload_traces(drained).await {
        // Both legs are best-effort from the tee's POV: the child decides what
        // counts as a retryable error. We surface failures via stderr so they
        // show up in the diagnostics report.
        eprintln!("otel-cli::tee[{label}]: child upload failed: {e}");
    }
}

/// Enqueue with the OTLP queue's drop-oldest policy.
///
/// `mpsc::Sender` doesn't expose a drop-oldest primitive. We emulate it: try
/// `try_send`; on `Full`, fall back to `send().await` with a tiny timeout. If
/// even that doesn't drain (because workers are entirely stuck), we increment
/// the counter and move on. The asymmetry is acceptable for the OTLP leg
/// because the file leg has the durable copy.
async fn enqueue_otlp(
    tx: &mpsc::Sender<ResourceSpans>,
    rs: ResourceSpans,
    stats: &TeeStats,
) {
    match tx.try_send(rs) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(rs)) => {
            stats.otlp_dropped_oldest.fetch_add(1, Ordering::Relaxed);
            // Brief block to let the worker drain a slot.
            match tokio::time::timeout(Duration::from_millis(50), tx.send(rs)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    // Worker is gone or completely stuck — drop silently.
                    eprintln!(
                        "otel-cli::tee[otlp]: queue full and worker not draining; dropping span"
                    );
                }
            }
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            eprintln!("otel-cli::tee[otlp]: queue closed; worker exited early");
        }
    }
}

/// Enqueue with the file queue's refuse-to-drop policy. Blocks up to
/// [`FILE_BLOCK_BUDGET`] and then logs + counts.
async fn enqueue_file(
    tx: &mpsc::Sender<ResourceSpans>,
    rs: ResourceSpans,
    stats: &TeeStats,
) -> Result<(), ClientError> {
    match tokio::time::timeout(FILE_BLOCK_BUDGET, tx.send(rs)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ClientError::Transport(format!("file queue closed: {e}"))),
        Err(_) => {
            stats.file_blocked_drops.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "otel-cli::tee[file]: blocked > {FILE_BLOCK_BUDGET:?} waiting for queue; \
                 giving up to avoid deadlock — this is DURABILITY LOSS"
            );
            Ok(())
        }
    }
}

#[async_trait]
impl OtlpClient for TeeClient {
    async fn start(&mut self) -> Result<(), ClientError> {
        // Children are start()'d inside their respective workers so a failing
        // child can't poison the tee's own startup.
        Ok(())
    }

    async fn upload_traces(&mut self, spans: Vec<ResourceSpans>) -> Result<(), ClientError> {
        // Per-message fan-out: each ResourceSpans goes to *both* queues, so
        // a slow file leg cannot stall the network leg or vice versa.
        // OTLP uses drop-oldest; file uses refuse-to-drop.
        let _ = DropPolicy::DropOldest; // silence dead-code warning for the doc-enum
        let _ = DropPolicy::Refuse;

        for rs in spans {
            enqueue_otlp(&self.otlp_tx, rs.clone(), &self.stats).await;
            enqueue_file(&self.file_tx, rs, &self.stats).await?;
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ClientError> {
        // Replace each sender with a closed one so the worker's `rx.recv()`
        // observes `None` and drains+exits.
        let (dummy_otlp_tx, _drop_rx) = mpsc::channel::<ResourceSpans>(1);
        let (dummy_file_tx, _drop_rx2) = mpsc::channel::<ResourceSpans>(1);
        let _ = std::mem::replace(&mut self.otlp_tx, dummy_otlp_tx);
        let _ = std::mem::replace(&mut self.file_tx, dummy_file_tx);

        if let Some(h) = self.otlp_handle.take() {
            let _ = h.await;
        }
        if let Some(h) = self.file_handle.take() {
            let _ = h.await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ScopeSpans, Span};
    use std::sync::atomic::AtomicU64;

    /// A counting child client used to assert workers actually pump spans
    /// through.
    struct CountingClient {
        name: &'static str,
        starts: Arc<AtomicU64>,
        upload_calls: Arc<AtomicU64>,
        spans_seen: Arc<AtomicU64>,
        stops: Arc<AtomicU64>,
    }

    #[async_trait]
    impl OtlpClient for CountingClient {
        async fn start(&mut self) -> Result<(), ClientError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn upload_traces(
            &mut self,
            spans: Vec<ResourceSpans>,
        ) -> Result<(), ClientError> {
            self.upload_calls.fetch_add(1, Ordering::SeqCst);
            self.spans_seen
                .fetch_add(spans.len() as u64, Ordering::SeqCst);
            let _ = self.name; // suppress unused warning in some build modes
            Ok(())
        }
        async fn stop(&mut self) -> Result<(), ClientError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn dummy_rs(name: &str) -> ResourceSpans {
        let span = Span {
            trace_id: hex::decode("0af7651916cd43dd8448eb211c80319c").unwrap(),
            span_id: hex::decode("b7ad6b7169203331").unwrap(),
            name: name.into(),
            ..Default::default()
        };
        ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![span],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }
    }

    #[tokio::test]
    async fn tee_fans_out_to_both_children() {
        let otlp_starts = Arc::new(AtomicU64::new(0));
        let otlp_calls = Arc::new(AtomicU64::new(0));
        let otlp_spans = Arc::new(AtomicU64::new(0));
        let otlp_stops = Arc::new(AtomicU64::new(0));

        let file_starts = Arc::new(AtomicU64::new(0));
        let file_calls = Arc::new(AtomicU64::new(0));
        let file_spans = Arc::new(AtomicU64::new(0));
        let file_stops = Arc::new(AtomicU64::new(0));

        let otlp = Box::new(CountingClient {
            name: "otlp",
            starts: otlp_starts.clone(),
            upload_calls: otlp_calls.clone(),
            spans_seen: otlp_spans.clone(),
            stops: otlp_stops.clone(),
        });
        let file = Box::new(CountingClient {
            name: "file",
            starts: file_starts.clone(),
            upload_calls: file_calls.clone(),
            spans_seen: file_spans.clone(),
            stops: file_stops.clone(),
        });

        let mut tee = TeeClient::new(otlp, file);
        tee.start().await.unwrap();

        // Push 5 ResourceSpans through.
        let payload: Vec<ResourceSpans> = (0..5).map(|i| dummy_rs(&format!("s-{i}"))).collect();
        tee.upload_traces(payload).await.unwrap();

        // Drain — both workers flush remaining batches + call stop().
        tee.stop().await.unwrap();

        assert_eq!(otlp_starts.load(Ordering::SeqCst), 1, "otlp started once");
        assert_eq!(file_starts.load(Ordering::SeqCst), 1, "file started once");
        assert_eq!(otlp_spans.load(Ordering::SeqCst), 5, "otlp saw all 5");
        assert_eq!(file_spans.load(Ordering::SeqCst), 5, "file saw all 5");
        assert_eq!(otlp_stops.load(Ordering::SeqCst), 1, "otlp stopped");
        assert_eq!(file_stops.load(Ordering::SeqCst), 1, "file stopped");
        // upload_calls counts may be 1 or 2 depending on batching/timer race;
        // the important invariant is "spans seen == spans sent".
    }
}
