//! Tokio mpsc + running-tasks counter. Equivalent of the Python
//! `QueueWithContext` (`__init__.py:1073-1086`): records both the number of
//! queued items and the number currently being processed, so the progress
//! reporter can distinguish "still queued" from "in flight".

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

#[derive(Debug)]
pub struct WorkQueue<T> {
    sender: mpsc::Sender<T>,
    receiver: tokio::sync::Mutex<mpsc::Receiver<T>>,
    queued: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
}

impl<T> WorkQueue<T> {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        Arc::new(Self {
            sender: tx,
            receiver: tokio::sync::Mutex::new(rx),
            queued: Arc::new(AtomicUsize::new(0)),
            running: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// A push-side handle that increments `queued` on enqueue.
    pub fn sender(self: &Arc<Self>) -> Sender<T> {
        Sender {
            inner: self.sender.clone(),
            queued: Arc::clone(&self.queued),
        }
    }

    /// Pop one item. Returns `None` when all senders have been dropped.
    /// While the returned `Guard` is alive, `running` is incremented; on
    /// drop it is decremented.
    pub async fn pop(self: &Arc<Self>) -> Option<Guard<T>> {
        let item = {
            let mut rx = self.receiver.lock().await;
            rx.recv().await
        }?;
        self.queued.fetch_sub(1, Ordering::AcqRel);
        self.running.fetch_add(1, Ordering::AcqRel);
        Some(Guard {
            item: Some(item),
            running: Arc::clone(&self.running),
        })
    }

    pub fn queued_len(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub fn running_len(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub struct Sender<T> {
    inner: mpsc::Sender<T>,
    queued: Arc<AtomicUsize>,
}

impl<T> Sender<T> {
    pub async fn send(&self, item: T) -> Result<(), mpsc::error::SendError<T>> {
        self.queued.fetch_add(1, Ordering::AcqRel);
        match self.inner.send(item).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Channel closed; undo the increment.
                self.queued.fetch_sub(1, Ordering::AcqRel);
                Err(e)
            }
        }
    }
}

/// RAII guard around a popped item. Decrements `running` on drop so the
/// progress reporter sees an accurate "in flight" count even on panics.
pub struct Guard<T> {
    item: Option<T>,
    running: Arc<AtomicUsize>,
}

impl<T> Guard<T> {
    pub fn into_inner(mut self) -> T {
        self.item.take().expect("guard already taken")
    }

    pub fn get(&self) -> &T {
        self.item.as_ref().expect("guard already taken")
    }
}

impl<T> Drop for Guard<T> {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::AcqRel);
    }
}
