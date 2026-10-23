//! Replay protection and rate limiting (spec §24).
//!
//! `ReplayGuard`: rejects duplicate `msg_id`s within a window (§10 replay
//! protection, applied outside sealed envelopes too).
//! `RateLimiter`: token bucket per source, for network transports.

use crate::Envelope;
use std::collections::HashMap;

#[derive(Debug)]
pub struct ReplayGuard {
    seen: HashMap<String, i64>,
    window_ms: i64,
}

impl ReplayGuard {
    pub fn new(window_ms: i64) -> Self {
        ReplayGuard { seen: HashMap::new(), window_ms }
    }

    /// Returns true if this msg_id is NEW within the window.
    /// A duplicate (or a re-appeared id inside the window) is rejected.
    pub fn check(&mut self, env: &Envelope, now_ms: i64) -> bool {
        // evict expired ids lazily every 1024 lookups (cheap amortization)
        if self.seen.len() % 1024 == 0 && !self.seen.is_empty() {
            self.seen.retain(|_, ts| now_ms - *ts <= self.window_ms);
        }
        if let Some(ts) = self.seen.get(&env.msg_id) {
            if now_ms - *ts <= self.window_ms {
                return false; // replay
            }
        }
        self.seen.insert(env.msg_id.clone(), now_ms);
        true
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Token bucket: `capacity` tokens, refilled at `refill_per_ms`.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: f64,
    tokens: f64,
    refill_per_ms: f64,
    last_refill: i64,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_ms: f64) -> Self {
        RateLimiter {
            capacity: capacity as f64,
            tokens: capacity as f64,
            refill_per_ms,
            last_refill: 0,
        }
    }

    /// Returns true if an action is allowed at `now_ms` (consumes 1 token).
    pub fn allow(&mut self, now_ms: i64) -> bool {
        let elapsed = (now_ms - self.last_refill).max(0) as f64;
        self.tokens = (self.tokens + elapsed * self.refill_per_ms).min(self.capacity);
        self.last_refill = now_ms;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aid, Body, Performative};

    fn env_with_id(id: &str) -> Envelope {
        let mut e = Envelope::new(
            Aid::local(1, "r"),
            Aid::new("did:x:y"),
            Performative::Request,
            Body::Text { text: "x".into() },
        );
        e.msg_id = id.into();
        e
    }

    #[test]
    fn replay_rejected_within_window() {
        let mut g = ReplayGuard::new(60_000);
        assert!(g.check(&env_with_id("a"), 1000));
        assert!(!g.check(&env_with_id("a"), 30_000)); // replay
        // outside the window: accepted again (id free)
        assert!(g.check(&env_with_id("a"), 61_001));
    }

    #[test]
    fn distinct_ids_pass() {
        let mut g = ReplayGuard::new(60_000);
        assert!(g.check(&env_with_id("a"), 1000));
        assert!(g.check(&env_with_id("b"), 1000));
        assert!(g.check(&env_with_id("c"), 1000));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn rate_limiter_bucket_semantics() {
        let mut rl = RateLimiter::new(3, 0.0); // no refill: pure bucket
        assert!(rl.allow(1000));
        assert!(rl.allow(1000));
        assert!(rl.allow(1000));
        assert!(!rl.allow(1000)); // empty
        assert!(!rl.allow(2000)); // still empty (no refill)
    }

    #[test]
    fn rate_limiter_refills() {
        // 1 token per ms: after 5ms elapsed, 5 tokens available (capped at 3)
        let mut rl = RateLimiter::new(3, 1.0);
        rl.last_refill = 0;
        assert!(rl.allow(10_000)); // bucket refills to 3
        assert!(rl.allow(10_000));
        assert!(rl.allow(10_000));
        assert!(!rl.allow(10_000));
        // 1ms later: exactly 1 token refilled
        assert!(rl.allow(10_001));
        assert!(!rl.allow(10_001));
    }
}
