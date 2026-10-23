//! Robustness: the decoder must NEVER panic on arbitrary bytes.
//! This is a fuzz-lite harness (in-CI); `fuzz/` contains the cargo-fuzz
//! target for deeper campaigns (needs nightly).

use acl_core::Envelope;

/// Deterministic PRNG so failures reproduce in CI.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
}

#[test]
fn decode_random_garbage_never_panics() {
    let mut rng = Lcg(0xa11c_e5e5_dead_1234);
    let mut buf = Vec::new();
    for _ in 0..50_000 {
        buf.clear();
        let len = (rng.next() % 512) as usize;
        for _ in 0..len {
            buf.push((rng.next() & 0xff) as u8);
        }
        // must return Result, never panic
        let _ = Envelope::decode(&buf);
    }
}

#[test]
fn decode_mutated_valid_envelopes_never_panics() {
    // start from a valid encoding and flip bits everywhere
    let mut env = Envelope::new(
        acl_core::Aid::local(1, "fuzz"),
        acl_core::Aid::new("did:x:y"),
        acl_core::Performative::Request,
        acl_core::Body::Task {
            description: "t".into(),
            input: serde_json::json!({"x": 1}),
            deadline_ms: None,
            priority: None,
        },
    );
    let valid = env.encode().unwrap();
    let mut rng = Lcg(0xfeed_face_cafe_babe);
    let mut mutated = valid.clone();
    for i in 0..20_000usize {
        let flips = 1 + (rng.next() % 8) as usize;
        mutated.copy_from_slice(&valid);
        for _ in 0..flips {
            let pos = (rng.next() as usize) % mutated.len();
            mutated[pos] ^= (rng.next() & 0xff) as u8;
        }
        let _ = Envelope::decode(&mutated);
        // also exercise verify path on decoded junk (must not panic)
        if let Ok(e) = Envelope::decode(&mutated) {
            env = e;
            let _ = env.is_expired(0);
        }
    }
}

#[test]
fn frame_boundary_values_never_panic() {
    // length headers claiming absurd sizes must be rejected, not OOM/panic
    for len in [0u32, 1, 0x7fff_ffff, u32::MAX] {
        let mut framed = len.to_be_bytes().to_vec();
        framed.extend_from_slice(&[0u8; 16]);
        // read_frame enforces the 16 MiB cap; feeding a slice is fine
        let _ = acl_core::read_frame(&mut &framed[..]);
    }
}
