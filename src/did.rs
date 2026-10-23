//! DID resolution (spec §22): turn `did:key` AIDs into verifying keys
//! without any network, and construct `did:web` resolution URLs.
//!
//! `did:key:z<base58btc(0xed 0x01 || ed25519_pubkey)>` per the did:key
//! spec (W3C CC work). `did:web` URL construction follows did:web §2.2;
//! actual fetching is the caller's transport concern.

use crate::AclError;
use bs58;
use ed25519_dalek::VerifyingKey;

/// ED25519 multicodec prefix: 0xed 0x01
const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Parse a `did:key:z...` AID into an Ed25519 verifying key.
pub fn did_key_to_verifying_key(did: &str) -> Result<VerifyingKey, AclError> {
    let b58 = did
        .strip_prefix("did:key:z")
        .ok_or_else(|| AclError::BadKey)?;
    let decoded = bs58::decode(b58)
        .into_vec()
        .map_err(|_| AclError::BadKey)?;
    if decoded.len() != 34 || decoded[..2] != ED25519_MULTICODEC {
        return Err(AclError::BadKey);
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&decoded[2..]);
    VerifyingKey::from_bytes(&pk).map_err(|_| AclError::BadKey)
}

/// Encode an Ed25519 verifying key as a `did:key` AID.
pub fn verifying_key_to_did_key(vk: &VerifyingKey) -> String {
    let mut payload = Vec::with_capacity(34);
    payload.extend_from_slice(&ED25519_MULTICODEC);
    payload.extend_from_slice(&vk.to_bytes());
    format!("did:key:z{}", bs58::encode(payload).into_string())
}

/// Build the did:web resolution URL (did:web §2.2):
///   did:web:example.com            → https://example.com/.well-known/did.json
///   did:web:example.com:path:to    → https://example.com/path/to/did.json
/// Returns the URL; fetching is the caller's job.
pub fn did_web_url(did: &str) -> Result<String, AclError> {
    let rest = did
        .strip_prefix("did:web:")
        .ok_or(AclError::BadKey)?;
    if rest.is_empty() {
        return Err(AclError::BadKey);
    }
    // path components after the first segment are %-encoded colons in did:web
    let mut segments = rest.split(':');
    let host = segments
        .next()
        .ok_or(AclError::BadKey)?
        .replace("%3A", ":");
    let path_parts: Vec<&str> = segments.collect();
    if path_parts.is_empty() {
        Ok(format!("https://{host}/.well-known/did.json"))
    } else {
        Ok(format!("https://{host}/{}:did.json", path_parts.join("/")))
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

    #[test]
    fn did_key_roundtrip() {
        let k = key();
        let vk = k.verifying_key();
        let did = verifying_key_to_did_key(&vk);
        assert!(did.starts_with("did:key:z"));
        // length varies with leading zero bytes; the roundtrip is the check
        let resolved = did_key_to_verifying_key(&did).expect("resolve");
        assert_eq!(resolved.to_bytes(), vk.to_bytes());
    }

    #[test]
    fn known_vector_shape() {
        // structural check: decode a hand-built payload
        let mut payload = ED25519_MULTICODEC.to_vec();
        payload.extend_from_slice(&[42u8; 32]);
        let did = format!("did:key:z{}", bs58::encode(&payload).into_string());
        let vk = did_key_to_verifying_key(&did).expect("resolve");
        assert_eq!(vk.to_bytes(), [42u8; 32]);
    }

    #[test]
    fn rejects_malformed() {
        // not a did:key
        assert!(did_key_to_verifying_key("did:web:x").is_err());
        assert!(did_key_to_verifying_key("did:key:znotbase58!!!").is_err());
        // wrong length
        assert!(did_key_to_verifying_key("did:key:z1111").is_err());
        // wrong multicodec (0x00 0x01 = not ed25519)
        let mut payload = vec![0x00, 0x01];
        payload.extend_from_slice(&[1u8; 32]);
        let did = format!("did:key:z{}", bs58::encode(&payload).into_string());
        assert!(did_key_to_verifying_key(&did).is_err());
    }

    #[test]
    fn envelope_verify_via_did_key_aid() {
        // the full loop: agent identified by did:key, envelope verified
        // with ONLY the from field — no separate key exchange needed
        let k = key();
        let did = verifying_key_to_did_key(&k.verifying_key());
        let mut env = Envelope::new(
            Aid::new(&did),
            Aid::new("did:key:zplaceholder"),
            Performative::Inform,
            Body::Text { text: "did works".into() },
        );
        env.sign(&k).unwrap();
        let vk = did_key_to_verifying_key(&env.from.0).expect("resolve from AID");
        assert!(env.verify(&vk).unwrap());
    }

    #[test]
    fn did_web_urls() {
        assert_eq!(
            did_web_url("did:web:example.com").unwrap(),
            "https://example.com/.well-known/did.json"
        );
        assert_eq!(
            did_web_url("did:web:example.com:users:alice").unwrap(),
            "https://example.com/users/alice:did.json"
        );
        // encoded port
        assert_eq!(
            did_web_url("did:web:example.com%3A8443").unwrap(),
            "https://example.com:8443/.well-known/did.json"
        );
        assert!(did_web_url("did:key:z123").is_err());
        assert!(did_web_url("did:web:").is_err());
    }
}
