//! Retry helpers shared by HTTP and gRPC clients.

use std::time::Duration;

/// Linear backoff with a hard cap on attempts.
pub fn backoff_for_attempt(attempt: u32) -> Duration {
    let secs = (attempt as u64).saturating_mul(1).min(30);
    Duration::from_secs(secs.max(1))
}
