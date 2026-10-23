//! Property-based tests (proptest): invariants that must hold for ANY input.

use acl_core::{Aid, Body, Envelope, Performative};
use proptest::prelude::*;

// ---- P1: encode → decode roundtrip preserves the envelope for arbitrary
// text fields (the common case: LLM-generated strings).

fn text_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec("[^\x00\x01]{0,200}", 0..10).prop_map(|v| v.join(""))
}

proptest! {
    #[test]
    fn roundtrip_arbitrary_text(
        text in text_strategy(),
        msg_id in "[a-f0-9]{32}",
        ttl in 1u32..3_600_000,
        hop in 0u8..8,
    ) {
        let mut env = Envelope::new(
            Aid::local(1, "p"),
            Aid::new("did:example:q"),
            Performative::Inform,
            Body::Text { text: text.clone() },
        );
        env.msg_id = msg_id;
        env.ttl = ttl;
        env.hop_count = hop;
        let bytes = env
            .encode()
            .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("encode: {e}")))?;
        let back = Envelope::decode(&bytes)
            .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("decode: {e}")))?;
        prop_assert_eq!(back.msg_id, env.msg_id);
        prop_assert_eq!(back.ttl, env.ttl);
        match back.body {
            Body::Text { text: t } => prop_assert_eq!(t, text),
            other => return Err(TestCaseError::fail(format!("wrong variant: {other:?}"))),
        }
    }

    // ---- P2: decode NEVER panics on arbitrary bytes (shrinking on failure).
    #[test]
    fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = Envelope::decode(&data);
    }

    // ---- P3: idempotency — the same msg_id is never accepted twice in a
    // window, regardless of the sequence of distinct ids interleaved.
    #[test]
    fn idempotency_no_duplicates(ids in proptest::collection::vec("[a-f0-9]{8}", 1..50)) {
        let mut cache = acl_core::idempotency::IdempotencyCache::new();
        let mut accepted = std::collections::HashSet::new();
        for id in ids {
            let mut e = Envelope::new(
                Aid::local(1, "p"),
                Aid::new("did:x:y"),
                Performative::Request,
                Body::Text { text: id.clone() },
            );
            e.msg_id = format!("prop-{id}");
            let first = cache.check(&e, 1000);
            if accepted.contains(&e.msg_id) {
                prop_assert!(!first, "id {} was accepted twice", e.msg_id);
            } else {
                prop_assert!(first, "first occurrence of {} rejected", e.msg_id);
                accepted.insert(e.msg_id.clone());
            }
        }
    }

    // ---- P4: encoding is byte-deterministic for identical envelopes.
    #[test]
    fn encode_is_deterministic(a in "[a-z ]{1,50}", ts in 0i64..i64::MAX) {
        let mut e1 = Envelope::new(
            Aid::local(1, "p"),
            Aid::new("did:x:y"),
            Performative::Inform,
            Body::Text { text: a.clone() },
        );
        let mut e2 = e1.clone();
        e1.ts = ts;
        e2.ts = ts;
        e1.msg_id = "prop-fixed-0001".into();
        e2.msg_id = "prop-fixed-0001".into();
        let b1 = e1
            .encode()
            .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("enc1: {e}")))?;
        let b2 = e2
            .encode()
            .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("enc2: {e}")))?;
        prop_assert_eq!(b1, b2);
    }
}
