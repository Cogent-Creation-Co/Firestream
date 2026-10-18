//! Bounded in-memory FIFO of stderr lines, sized for a tail-N preview pane.
//!
//! The producer is the per-line tee loop in the build (and eval) stderr
//! drain; the consumer is a live UI's render thread that snapshots the
//! ring every render tick. `std::sync::Mutex` because the consumer is
//! synchronous and the producer's hold is microsecond-scale.

use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug)]
pub struct LineRing {
    inner: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LineRing {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(n)),
            capacity: n.max(1),
        }
    }

    /// Append `line`, evicting the oldest entry if the ring is at capacity.
    pub fn push(&self, line: String) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        if g.len() == self.capacity {
            g.pop_front();
        }
        g.push_back(line);
    }

    /// Snapshot current contents (oldest-first). Cheap clone of small Vec.
    pub fn snapshot(&self) -> Vec<String> {
        match self.inner.lock() {
            Ok(g) => g.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_under_capacity_appends() {
        let r = LineRing::with_capacity(4);
        r.push("a".into());
        r.push("b".into());
        assert_eq!(r.snapshot(), vec!["a", "b"]);
    }

    #[test]
    fn push_at_capacity_evicts_oldest() {
        let r = LineRing::with_capacity(3);
        for s in ["a", "b", "c", "d", "e"] {
            r.push(s.into());
        }
        assert_eq!(r.snapshot(), vec!["c", "d", "e"]);
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let r = LineRing::with_capacity(0);
        r.push("a".into());
        r.push("b".into());
        assert_eq!(r.snapshot(), vec!["b"]);
    }

    #[test]
    fn is_empty_reflects_state() {
        let r = LineRing::with_capacity(2);
        assert!(r.is_empty());
        r.push("a".into());
        assert!(!r.is_empty());
    }
}
