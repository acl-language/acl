//! Sealed envelopes (spec §10.1): end-to-end confidentiality through relays.
//!
//! `seal` takes an inner envelope and produces an outer envelope whose body
//! is `Body::Sealed`: the inner envelope's msgpack encoding, encrypted with
//! ChaCha20-Poly1305 under a key derived via X25519 ECDH(sender_secret,
//! recipient_public) + HKDF-ish salt. `open` reverses it.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use rand::Rng;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{AclError, Body, Envelope, Performative, Aid};

/// Seal `inner` so only the holder of `recipient_secret`'s counterpart
/// public key can open it. The outer envelope keeps routing metadata
/// (from/to/performative=inform) but the payload is opaque in transit.
pub fn seal(
    inner: &Envelope,
    sender_secret: &StaticSecret,
    recipient_public: &PublicKey,
) -> Result<Envelope, AclError> {
    let shared = sender_secret.diffie_hellman(recipient_public);
    let mut salt = [0u8; 12];
    rand::rng().fill_bytes(&mut salt);

    // §10.1 (v0.2): HKDF-SHA256 extraction and expansion — the raw shared
    // secret is NEVER used directly as the symmetric key.
    // Salt: the 12-byte random salt (domain-separated from the nonce use
    // via the info label). Info: "acl-sealed-v1-key".
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"acl-sealed-v1-key", &mut key)
        .expect("valid key length");
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plaintext = inner.encode()?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&salt),
            Payload {
                msg: &plaintext,
                aad: b"acl-sealed-v1",
            },
        )
        .map_err(|_| AclError::BadKey)?;

    let mut env = Envelope::new(
        inner.from.clone(),
        inner.to.clone(),
        Performative::Inform,
        Body::Sealed {
            sender_pub: PublicKey::from(sender_secret).to_bytes().to_vec(),
            salt: salt.to_vec(),
            ciphertext: ciphertext.into(),
        },
    );
    env.msg_id = inner.msg_id.clone();
    env.reply_with = inner.reply_with.clone();
    Ok(env)
}

/// Open a sealed envelope. `recipient_secret` must pair with the sender
/// public key embedded in the Sealed body.
pub fn open(
    sealed: &Envelope,
    recipient_secret: &StaticSecret,
) -> Result<Envelope, AclError> {
    let (sender_pub, salt, ciphertext) = match &sealed.body {
        Body::Sealed { sender_pub, salt, ciphertext } => (sender_pub, salt, ciphertext),
        _ => return Err(AclError::BadKey),
    };
    let sender_pk = PublicKey::from(
        <[u8; 32]>::try_from(sender_pub.as_slice()).map_err(|_| AclError::BadKey)?,
    );
    let shared = recipient_secret.diffie_hellman(&sender_pk);
    // §10.1 (v0.2): mirror the sender's HKDF derivation using the salt
    // embedded in the sealed body.
    let hk = Hkdf::<Sha256>::new(Some(salt), shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"acl-sealed-v1-key", &mut key)
        .expect("valid key length");
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(salt),
            Payload {
                msg: ciphertext,
                aad: b"acl-sealed-v1",
            },
        )
        .map_err(|_| AclError::BadKey)?;
    Envelope::decode(&plaintext)
}


/// §10.2: Derive a session-specific symmetric key from a root secret.
/// Used to derive ephemeral keys for specific message streams (e.g. per
/// agent-pair, per task-id) without re-running the full ECDH handshake
/// for every message (reduces CPU overhead and exposure).
/// `root_secret` must be the 32-byte ECDH shared secret.
/// `session_id` is an opaque string distinguishing the channel (e.g.
/// "agent-a-to-b-task-123").
pub fn derive_session_key(root_secret: &[u8], session_id: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, root_secret);
    let mut key = [0u8; 32];
    // Info binds the derivation to the exact channel identity and purpose.
    let info = format!("acl-session-v1-{}", session_id);
    hk.expand(info.as_bytes(), &mut key)
        .expect("valid key length");
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Aid;
    use rand::Rng;

    fn inner() -> Envelope {
        Envelope::new(
            Aid::new("did:example:a"),
            Aid::new("did:example:b"),
            Performative::Request,
            Body::Task {
                description: "secret task".into(),
                input: serde_json::json!({"text": "classified"}),
                deadline_ms: None,
                priority: None,
            },
        )
    }

    fn random_secret() -> StaticSecret {
        let mut b = [0u8; 32];
        rand::rng().fill_bytes(&mut b);
        StaticSecret::from(b)
    }

    #[test]
    fn seal_open_roundtrip() {
        let sender = random_secret();
        let recipient = random_secret();
        let recipient_pub = PublicKey::from(&recipient);
        let task = inner();

        let sealed = seal(&task, &sender, &recipient_pub).unwrap();
        // sealed body must NOT leak the plaintext
        match &sealed.body {
            Body::Sealed { ciphertext, .. } => {
                assert!(!serde_json::to_string(&ciphertext).unwrap().contains("classified"));
            }
            _ => panic!("expected Sealed"),
        }
        let opened = open(&sealed, &recipient).unwrap();
        assert_eq!(opened.msg_id, task.msg_id);
        match &opened.body {
            Body::Task { input, .. } => assert_eq!(input["text"], "classified"),
            _ => panic!("expected Task"),
        }
    }

    #[test]
    fn wrong_key_fails() {
        let sender = random_secret();
        let eavesdropper = random_secret();
        let recipient_pub = PublicKey::from(&random_secret());
        let sealed = seal(&inner(), &sender, &recipient_pub).unwrap();
        assert!(open(&sealed, &eavesdropper).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let sender = random_secret();
        let recipient = random_secret();
        let mut sealed = seal(&inner(), &sender, &PublicKey::from(&recipient)).unwrap();
        if let Body::Sealed { ciphertext, .. } = &mut sealed.body {
            ciphertext[0] ^= 0xff;
        }
        assert!(open(&sealed, &recipient).is_err());
    }

    #[test]
    fn session_key_derivation_is_deterministic_and_distinct() {
        let secret = random_secret();
        let shared = secret.diffie_hellman(&PublicKey::from(&random_secret()));
        let root = shared.as_bytes();

        let k1 = derive_session_key(root, "session-a");
        let k2 = derive_session_key(root, "session-a");
        let k3 = derive_session_key(root, "session-b");

        // deterministic: same input -> same key
        assert_eq!(k1, k2);
        // distinct sessions -> distinct keys
        assert_ne!(k1, k3);
    }
}
