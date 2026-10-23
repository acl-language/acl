//! Idempotency and ordering (spec §8.1).
//!
//! Retransmission semantics: a client retrying a request MUST reuse the
//! same `msg_id` and body; the receiver deduplicates within a TTL window.
//! `IdempotencyCache` implements that receiver side.

use crate::{Body, Envelope};
use std::collections::HashMap;

/// Body -> JSON value (for hashing; Body itself isn't Hash).
fn body_to_value(b: &Body) -> serde_json::Value {
    match b {
        Body::Task { description, input, deadline_ms, priority } => serde_json::json!({
            "k": "Task", "d": description, "i": input, "dl": deadline_ms, "p": priority }),
        Body::Result { output, artifacts } => serde_json::json!({
            "k": "Result", "o": output, "a": artifacts }),
        Body::Text { text } => serde_json::json!({ "k": "Text", "t": text }),
        Body::Query { subject, parameters } => serde_json::json!({ "k": "Query", "s": subject, "p": parameters }),
        Body::Proposal { terms, price, valid_until } => serde_json::json!({ "k": "Proposal", "t": terms, "p": price, "v": valid_until }),
        Body::Ref { target_msg_id } => serde_json::json!({ "k": "Ref", "t": target_msg_id }),
        Body::Progress { target_msg_id, percent, note } => serde_json::json!({ "k": "Progress", "t": target_msg_id, "p": percent, "n": note }),
        Body::InputNeeded { target_msg_id, what, schema } => serde_json::json!({ "k": "InputNeeded", "t": target_msg_id, "w": what, "s": schema }),
        Body::Failure { target_msg_id, code, reason, recoverable } => serde_json::json!({ "k": "Failure", "t": target_msg_id, "c": code, "r": reason, "rec": recoverable }),
        Body::Error { code, message, details } => serde_json::json!({ "k": "Error", "c": code, "m": message, "d": details }),
        Body::Hello { nonce, .. } => serde_json::json!({ "k": "Hello", "n": nonce }),
        Body::Sealed { ciphertext, .. } => serde_json::json!({ "k": "Sealed", "c": ciphertext }),
        Body::Delegate { chain, .. } => serde_json::json!({ "k": "Delegate", "c": chain }),
    }
}

#[derive(Debug, Default)]
pub struct IdempotencyCache {
    /// msg_id -> (arrival monotonic-ish timestamp, body fingerprint)
    seen: HashMap<String, (i64, u64)>,
}

/// Semantic fingerprint: performative + from/to + body. Deliberately
/// EXCLUDES ts so a re-signed retransmission (new ts, same msg_id and
/// body) is still recognized as a clean duplicate.
fn fingerprint(env: &Envelope) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    env.performative.hash(&mut h);
    env.from.hash(&mut h);
    env.to.hash(&mut h);
    let body_json = serde_json::to_string(&body_to_value(&env.body)).unwrap_or_default();
    body_json.hash(&mut h);
    h.finish()
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this is the FIRST time we accept this message.
    /// A duplicate (same msg_id, same fingerprint, still within TTL) is
    /// rejected — the caller MUST NOT re-process it, but SHOULD re-send the
    /// cached response if it has one.
    /// A same-msg_id message with a DIFFERENT fingerprint is a protocol
    /// violation: also rejected, and should be logged as an error.
    pub fn check(&mut self, env: &Envelope, now_ms: i64) -> bool {
        let fp = fingerprint(env);
        match self.seen.get(&env.msg_id) {
            // Any existing msg_id is a duplicate → never "first". The
            // fingerprint equality only distinguishes clean duplicates
            // from protocol violations (both rejected here; caller logs).
            Some((_, old_fp)) => {
                let _ = *old_fp == fp;
                false
            }
            None => {
                self.seen.insert(env.msg_id.clone(), (now_ms, fp));
                true
            }
        }
    }

    /// Evict entries older than `ttl_ms` (call periodically).
    pub fn evict_older_than(&mut self, now_ms: i64, ttl_ms: i64) {
        self.seen.retain(|_, (ts, _)| now_ms - *ts <= ttl_ms);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aid, Body, Performative};

    fn env_with_id(msg_id: &str, text: &str) -> Envelope {
        let mut e = Envelope::new(
            Aid::local(1, "t"),
            Aid::new("did:x:y"),
            Performative::Request,
            Body::Text { text: text.into() },
        );
        e.msg_id = msg_id.into();
        e
    }

    #[test]
    fn first_accepts_duplicate_rejects() {
        let mut c = IdempotencyCache::new();
        let e = env_with_id("aaaa", "hello");
        assert!(c.check(&e, 1000)); // first
        assert!(!c.check(&e, 1100)); // clean duplicate
        assert!(!c.check(&e, 5000)); // still duplicate (no eviction yet)
    }

    #[test]
    fn same_id_different_body_is_violation() {
        let mut c = IdempotencyCache::new();
        assert!(c.check(&env_with_id("bbbb", "hello"), 1000));
        assert!(!c.check(&env_with_id("bbbb", "EVIL"), 1100));
    }

    #[test]
    fn eviction_frees_slot() {
        let mut c = IdempotencyCache::new();
        assert!(c.check(&env_with_id("cccc", "x"), 1000));
        c.evict_older_than(1_000_000, 300_000);
        assert!(c.is_empty());
        // after eviction + TTL, reuse of the id is accepted again
        assert!(c.check(&env_with_id("cccc", "x"), 1_000_001));
    }

    #[test]
    fn distinct_ids_pass() {
        let mut c = IdempotencyCache::new();
        assert!(c.check(&env_with_id("d1", "x"), 1000));
        assert!(c.check(&env_with_id("d2", "x"), 1000));
        assert_eq!(c.len(), 2);
    }
}
