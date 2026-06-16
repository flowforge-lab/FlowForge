//! Capped exponential backoff for MCP server restarts (RFC 0003 §5).
//!
//! Pure and side-effect free so the schedule is unit-testable without sleeping: the
//! supervisor calls [`Backoff::next_delay`] to decide how long to wait before the next
//! restart attempt and [`Backoff::reset`] once a server is healthy again.

use std::time::Duration;

/// Restart backoff policy: `base * 2^attempt`, clamped to `max`.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    /// A backoff growing from `base`, doubling each attempt, never exceeding `max`.
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    /// The delay before the next attempt, advancing the schedule. The first call
    /// returns `base`; subsequent calls double until clamped at `max`.
    pub fn next_delay(&mut self) -> Duration {
        // Saturating shift: a large attempt count can't overflow into a tiny value.
        let factor = 1u64.checked_shl(self.attempt).unwrap_or(u64::MAX);
        let scaled = self
            .base
            .checked_mul(factor.min(u32::MAX as u64) as u32)
            .unwrap_or(self.max);
        self.attempt = self.attempt.saturating_add(1);
        scaled.min(self.max)
    }

    /// Reset to the start of the schedule after a successful (re)connect.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn doubles_from_base() {
        let mut b = Backoff::new(ms(500), ms(30_000));
        assert_eq!(b.next_delay(), ms(500));
        assert_eq!(b.next_delay(), ms(1_000));
        assert_eq!(b.next_delay(), ms(2_000));
        assert_eq!(b.next_delay(), ms(4_000));
    }

    #[test]
    fn clamps_at_max() {
        let mut b = Backoff::new(ms(500), ms(3_000));
        let seq: Vec<_> = (0..6).map(|_| b.next_delay()).collect();
        assert_eq!(
            seq,
            vec![
                ms(500),
                ms(1_000),
                ms(2_000),
                ms(3_000),
                ms(3_000),
                ms(3_000)
            ]
        );
    }

    #[test]
    fn reset_returns_to_base() {
        let mut b = Backoff::new(ms(500), ms(30_000));
        b.next_delay();
        b.next_delay();
        b.reset();
        assert_eq!(b.next_delay(), ms(500));
    }

    #[test]
    fn extreme_attempt_count_saturates_to_max() {
        let mut b = Backoff::new(ms(500), ms(30_000));
        for _ in 0..100 {
            let d = b.next_delay();
            assert!(d <= ms(30_000));
        }
    }
}
