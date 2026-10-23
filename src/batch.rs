//! Batch signature verification (spec §BENCH note): relays and registries
//! verify many envelopes from the same or different senders; the batched
//! ed25519 algorithm amortizes the expensive scalar math across signatures.

use crate::Envelope;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Verify many (envelope, key) pairs. Returns Ok(()) only if every
/// signature is valid; Err lists the indices of the invalid ones.
///
/// Falls back to per-item verification if the batched API rejects the set
/// (the batched algorithm can false-negative on adversarial input; the
/// fallback then gives exact per-item results).
pub fn verify_batch(pairs: &[(&Envelope, &VerifyingKey)]) -> Result<(), Vec<usize>> {
    let mut bad = Vec::new();

    #[cfg(feature = "batch")]
    {
        use ed25519_dalek::batch::Verifier as BatchVerifier;
        let mut batch = ed25519_dalek::batch::BatchVerifier::new();
        for (i, (env, vk)) in pairs.iter().enumerate() {
            if let Some(sig_bytes) = env.sig {
                let unsigned = env.encode_unsigned().unwrap_or_default();
                match Signature::try_from(&sig_bytes[..]) {
                    Ok(sig) => batch.queue((*vk, sig, unsigned)),
                    Err(_) => bad.push(i),
                }
            } else {
                bad.push(i);
            }
        }
        if bad.is_empty() {
            match batch.verify() {
                Ok(()) => return Ok(()),
                Err(_) => {
                    // batch rejected: fall through to exact per-item check
                }
            }
        } else {
            return Err(bad);
        }
    }

    // exact per-item pass (also the fallback path)
    for (i, (env, vk)) in pairs.iter().enumerate() {
        let ok = env
            .sig
            .as_ref()
            .and_then(|sig_bytes| {
                Signature::try_from(&sig_bytes[..])
                    .ok()
                    .and_then(|sig| vk.verify(&env.encode_unsigned().ok()?, &sig).ok())
            })
            .is_some();
        if !ok && !bad.contains(&i) {
            bad.push(i);
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aid, Body, Envelope, Performative};
    use ed25519_dalek::SigningKey;
    use rand::Rng;

    fn key() -> SigningKey {
        let mut s = [0u8; 32];
        rand::rng().fill_bytes(&mut s);
        SigningKey::from_bytes(&s)
    }

    fn signed_env(key: &SigningKey, text: &str) -> Envelope {
        let mut env = Envelope::new(
            Aid::local(1, "b"),
            Aid::new("did:x:y"),
            Performative::Inform,
            Body::Text { text: text.into() },
        );
        env.sign(key).unwrap();
        env
    }

    #[test]
    fn all_valid_batch_passes() {
        let k1 = key();
        let k2 = key();
        let envs = vec![signed_env(&k1, "a"), signed_env(&k2, "b"), signed_env(&k1, "c")];
        let vk1 = k1.verifying_key();
        let vk2 = k2.verifying_key();
        let pairs: Vec<(&Envelope, &VerifyingKey)> = vec![
            (&envs[0], &vk1),
            (&envs[1], &vk2),
            (&envs[2], &vk1),
        ];
        assert!(verify_batch(&pairs).is_ok());
    }

    #[test]
    fn one_tampered_fails_with_index() {
        let k1 = key();
        let mut good = signed_env(&k1, "good");
        let mut tampered = signed_env(&k1, "tamper-me");
        tampered.body = Body::Text { text: "tampered!".into() };
        let vk = k1.verifying_key();
        let pairs = vec![(&good, &vk), (&tampered, &vk)];
        match verify_batch(&pairs) {
            Err(bad) => assert_eq!(bad, vec![1]),
            Ok(()) => panic!("tampered envelope passed batch"),
        }
        let _ = good; // keep alive
    }

    #[test]
    fn missing_signature_fails() {
        let k1 = key();
        let unsigned = Envelope::new(
            Aid::local(1, "b"),
            Aid::new("did:x:y"),
            Performative::Inform,
            Body::Text { text: "no sig".into() },
        );
        let signed = signed_env(&k1, "signed");
        let vk = k1.verifying_key();
        let pairs = vec![(&signed, &vk), (&unsigned, &vk)];
        match verify_batch(&pairs) {
            Err(bad) => assert_eq!(bad, vec![1]),
            Ok(()) => panic!("unsigned envelope passed"),
        }
    }

    #[test]
    fn empty_batch_ok() {
        assert!(verify_batch(&[]).is_ok());
    }
}
