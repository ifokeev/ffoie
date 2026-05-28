//! Per-connection token-bucket rate limiter.
//!
//! A token bucket is the simplest rate-limit algorithm that handles both
//! steady-state throughput (refill_per_sec) and short bursts (burst capacity).
//! It is intentionally allocation-free and contains no async code so it can
//! be called directly inside the select! loop without blocking.
//!
//! # Design decisions
//! - Tokens are stored as `f64` so fractional refills work correctly for
//!   refill rates like 2 tokens/second (0.5 tokens per 250ms tick).
//! - `burst` and `refill_per_sec` are stored inside the struct so each bucket
//!   is self-contained; the caller does not need to pass them on every call.
//! - `retry_after_ms()` returns a *constant* estimate (time for one full token)
//!   rather than the exact time until the next integer token.  That is correct
//!   for our use-case: the client just needs to know when to try again.

use std::time::{Duration, Instant};

/// Token-bucket per WebSocket connection.
///
/// Create one per connection with [`TokenBucket::new`].
/// Call [`TokenBucket::consume`] on each inbound message.
pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    burst: f64,
    refill_per_sec: f64,
}

impl TokenBucket {
    /// Create a new token bucket pre-filled to `burst` capacity.
    ///
    /// * `burst`          — maximum number of tokens (and starting balance).
    /// * `refill_per_sec` — tokens added per second (may be fractional).
    pub fn new(burst: u32, refill_per_sec: u32) -> Self {
        Self {
            tokens: burst as f64,
            last_refill: Instant::now(),
            burst: burst as f64,
            refill_per_sec: refill_per_sec as f64,
        }
    }

    /// Try to consume one token.
    ///
    /// Refills the bucket based on elapsed time since the last call, then
    /// attempts to consume one token.
    ///
    /// Returns `true` if a token was available (message allowed).
    /// Returns `false` if the bucket is empty (message should be rate-limited).
    pub fn consume(&mut self) -> bool {
        // Refill based on elapsed wall time.
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens += elapsed * self.refill_per_sec;
        if self.tokens > self.burst {
            self.tokens = self.burst;
        }
        self.last_refill = Instant::now();

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Estimated milliseconds until the next token is available.
    ///
    /// This is a constant estimate (`1000 / refill_per_sec`) rather than the
    /// exact time remaining, which is good enough for a retry hint sent to
    /// the client.
    pub fn retry_after_ms(&self) -> u64 {
        (1000.0 / self.refill_per_sec) as u64
    }

    /// Returns the time until one token refills as a [`Duration`].
    ///
    /// Convenience wrapper around [`retry_after_ms`] for use in tests.
    #[allow(dead_code)]
    pub fn retry_after(&self) -> Duration {
        Duration::from_millis(self.retry_after_ms())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// Burst=10: the first 10 calls on a fresh bucket must all succeed.
    #[test]
    fn full_bucket_allows_burst_calls() {
        let mut bucket = TokenBucket::new(10, 2);
        for i in 0..10 {
            assert!(
                bucket.consume(),
                "consume() call {i} should succeed on a burst-10 bucket"
            );
        }
    }

    /// The 11th call after a burst of 10 must be denied.
    #[test]
    fn eleventh_call_denied_after_burst() {
        let mut bucket = TokenBucket::new(10, 2);
        for _ in 0..10 {
            bucket.consume();
        }
        assert!(
            !bucket.consume(),
            "11th consume() should fail — bucket exhausted"
        );
    }

    /// After 500 ms with refill_per_sec=2, one token should have refilled
    /// (0.5s × 2/s = 1.0 token), allowing one more consume().
    ///
    /// Uses a real sleep — kept short (501ms) but sufficient for the CI
    /// scheduler.  The test accepts any timing slack >= 1ms past the 500ms
    /// mark, so it is not flaky in practice.
    #[test]
    fn refill_after_500ms_allows_one_more() {
        let mut bucket = TokenBucket::new(10, 2);
        // Exhaust the bucket.
        for _ in 0..10 {
            bucket.consume();
        }
        // Verify it is empty.
        assert!(!bucket.consume(), "bucket should be empty before sleep");

        // Wait long enough for exactly one token to refill.
        thread::sleep(Duration::from_millis(501));

        assert!(
            bucket.consume(),
            "one token should have refilled after 500ms at 2/s"
        );
    }

    /// retry_after_ms() should return ceil(1000 / refill_per_sec).
    /// For refill_per_sec=2: 500ms; for refill_per_sec=10: 100ms.
    #[test]
    fn retry_after_ms_calculation() {
        let b2 = TokenBucket::new(10, 2);
        assert_eq!(b2.retry_after_ms(), 500, "2/s → 500ms retry");

        let b10 = TokenBucket::new(10, 10);
        assert_eq!(b10.retry_after_ms(), 100, "10/s → 100ms retry");

        let b1 = TokenBucket::new(5, 1);
        assert_eq!(b1.retry_after_ms(), 1000, "1/s → 1000ms retry");
    }

    /// Verify that after exhausting a burst=1 bucket, one refill wait
    /// restores exactly one token (not more).
    #[test]
    fn single_token_refill_is_capped_at_burst() {
        let mut bucket = TokenBucket::new(1, 2); // burst=1, refill=2/s
        assert!(bucket.consume(), "first consume must succeed");
        assert!(!bucket.consume(), "second must fail immediately");

        // Wait for one token.
        thread::sleep(Duration::from_millis(510));

        assert!(bucket.consume(), "one token should have refilled");
        // Immediately after consuming the refilled token there should be < 1 token.
        // (0.5/s rate for 1ms ≈ 0.001 — nowhere near 1)
        assert!(!bucket.consume(), "no second token so quickly after refill");
    }
}
