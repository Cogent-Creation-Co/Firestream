//! Pattern #7 — exponential-backoff pull retry. Mirrors
//! `bin/_lib.sh::docker_pull_retry` (lines 120-133): 3 attempts, base 5s
//! delay, double on each retry.

use std::time::Duration;

use bollard::image::CreateImageOptions;
use futures::TryStreamExt;

use super::{DockerClient, Error};

/// Retry policy. `attempts` is the max number of tries (1 = no retry).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // Matches `_lib.sh::docker_pull_retry`: max=3, delay=5.
        Self {
            attempts: 3,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let factor = 2u64.saturating_pow(attempt.saturating_sub(1));
        let d = self.base_delay.saturating_mul(factor as u32);
        if d > self.max_delay {
            self.max_delay
        } else {
            d
        }
    }
}

/// Pull an image via bollard with retry-on-failure. Bollard yields a stream
/// of progress events; we drain it and only count the operation as
/// successful if the stream completes without error.
///
/// `platform` is e.g. "linux/amd64" / "linux/arm64". Empty string ⇒ use
/// the daemon's default.
pub async fn pull_with_retry(
    client: &DockerClient,
    image: &str,
    platform: &str,
    policy: RetryPolicy,
) -> Result<(), Error> {
    let mut last_err: Option<Error> = None;
    for attempt in 1..=policy.attempts {
        let options = CreateImageOptions {
            from_image: image.to_string(),
            platform: platform.to_string(),
            ..Default::default()
        };
        let result = client
            .inner()
            .create_image(Some(options), None, None)
            .try_collect::<Vec<_>>()
            .await;

        match result {
            Ok(_) => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    target: "firestream_ci::oci",
                    attempt, attempts = policy.attempts, image, error = ?e,
                    "docker pull failed; retrying after backoff"
                );
                last_err = Some(Error::Bollard(e));
                if attempt < policy.attempts {
                    tokio::time::sleep(policy.delay_for_attempt(attempt)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or(Error::NotFound(image.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_matches_bash() {
        let p = RetryPolicy::default();
        assert_eq!(p.attempts, 3);
        assert_eq!(p.base_delay, Duration::from_secs(5));
    }

    #[test]
    fn backoff_doubles_per_attempt() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(5));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(10));
        assert_eq!(p.delay_for_attempt(3), Duration::from_secs(20));
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        let p = RetryPolicy {
            attempts: 10,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(30),
        };
        assert_eq!(p.delay_for_attempt(10), Duration::from_secs(30));
    }
}
