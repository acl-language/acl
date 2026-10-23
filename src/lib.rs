//! ACL — Agent Communication Language, core codec (spec v0.1).
//!
//! Implements L1–L4 of SPEC.md: MessagePack envelope, performatives,
//! body variants, Ed25519 signing, AgentCard, stream framing.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
pub mod bridge;
pub mod sealed;
pub mod idempotency;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::io::Read;

pub const ACL_VERSION: u8 = 1;
pub const MAX_HOP_COUNT: u8 = 8;
pub const DEFAULT_TTL_MS: u32 = 300_000;

// ---------------------------------------------------------------- performatives

/// Speech acts (spec §5). Wire representation: u8 discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Performative {
    Request = 1,
    Inform = 2,
    Query = 3,
    Propose = 4,
    Accept = 5,
    Reject = 6,
    Cancel = 7,
    Progress = 8,
    InputRequired = 9,
    Failure = 10,
    Delegate = 11,
    Error = 12,
    Hello = 13,
    Bye = 14,
}

/// Artifact attached to a `Result` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Artifact {
    pub name: String,
    pub mime: String,
    /// Raw bytes, encoded as msgpack bin on the wire.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// Body variants (spec §6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", content = "v")]
pub enum Body {
    Task {
        description: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
    },
    Result {
        output: serde_json::Value,
        #[serde(default)]
        artifacts: Vec<Artifact>,
    },
    Text {
        text: String,
    },
    Query {
        subject: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameters: Option<serde_json::Value>,
    },
    Proposal {
        terms: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        price: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        valid_until: Option<i64>,
    },
    Ref {
        target_msg_id: String,
    },
    Progress {
        target_msg_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    InputNeeded {
        target_msg_id: String,
        what: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
    },
    Failure {
        target_msg_id: String,
        code: u16,
        reason: String,
        recoverable: bool,
    },
    Error {
        code: u16,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    Hello {
        agent_card: Option<AgentCard>,
        card_hash: Option<String>,
        nonce: Vec<u8>,
    },
    Delegate {
        task: Box<Body>, // must be Body::Task
        chain: Vec<String>,
    },
    /// §10.1: encrypted inner envelope (X25519 + ChaCha20-Poly1305)
    Sealed {
        sender_pub: Vec<u8>,
        salt: Vec<u8>,
        ciphertext: Vec<u8>,
    },
}

// ---------------------------------------------------------------- identity

/// Agent ID (spec §4.1): `scheme:opaque`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Aid(pub String);

impl Aid {
    pub fn new(s: impl Into<String>) -> Self {
        Aid(s.into())
    }
    pub fn scheme(&self) -> Option<&str> {
        self.0.split(':').next()
    }
    pub fn local(pid: u32, session: &str) -> Self {
        Aid(format!("local:{pid}:{session}"))
    }
}

// ---------------------------------------------------------------- envelope

/// The ACL envelope (spec §4). Field order is wire order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub acl: u8,
    /// 128-bit id, hex-encoded (32 chars).
    pub msg_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub from: Aid,
    pub to: Aid,
    /// unix milliseconds UTC
    pub ts: i64,
    #[serde(default = "default_ttl")]
    pub ttl: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_with: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hop_count: u8,
    pub performative: Performative,
    pub body: Body,
    /// Ed25519 signature over the encoded envelope with `sig` absent.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_serde_bytes")]
    pub sig: Option<Vec<u8>>,
}

fn default_ttl() -> u32 {
    DEFAULT_TTL_MS
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// Generate a random 128-bit hex msg_id.
pub fn new_msg_id() -> String {
    use rand::Rng;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    hex(&b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

mod opt_serde_bytes {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => serde_bytes::serialize(b, s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let opt: Option<serde_bytes::ByteBuf> = serde_bytes::deserialize(d)?;
        Ok(opt.map(|b| b.into_vec()))
    }
}

impl Envelope {
    pub fn new(from: Aid, to: Aid, performative: Performative, body: Body) -> Self {
        Envelope {
            acl: ACL_VERSION,
            msg_id: new_msg_id(),
            in_reply_to: None,
            from,
            to,
            ts: now_ms(),
            ttl: DEFAULT_TTL_MS,
            reply_with: None,
            hop_count: 0,
            performative,
            body,
            sig: None,
        }
    }

    /// Serialize the unsigned envelope (what the signature covers).
    pub fn encode_unsigned(&self) -> Result<Vec<u8>, AclError> {
        let mut copy = self.clone();
        copy.sig = None;
        rmp_serde::to_vec_named(&copy).map_err(AclError::Encode)
    }

    /// Full wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, AclError> {
        rmp_serde::to_vec_named(self).map_err(AclError::Encode)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AclError> {
        let env: Envelope = rmp_serde::from_slice(bytes).map_err(AclError::Decode)?;
        if env.acl != ACL_VERSION {
            return Err(AclError::Version(env.acl));
        }
        Ok(env)
    }

    /// Sign with an Ed25519 key. `sig` covers the envelope without `sig`.
    pub fn sign(&mut self, key: &SigningKey) -> Result<(), AclError> {
        let unsigned = self.encode_unsigned()?;
        self.sig = Some(key.sign(&unsigned).to_bytes().to_vec());
        Ok(())
    }

    /// Verify the signature against a public key.
    pub fn verify(&self, vk: &VerifyingKey) -> Result<bool, AclError> {
        let Some(sig_bytes) = self.sig.clone() else {
            return Ok(false);
        };
        let sig = Signature::from_bytes(
            <&[u8; 64]>::try_from(&sig_bytes[..]).map_err(|_| AclError::BadKey)?,
        );
        let unsigned = self.encode_unsigned()?;
        Ok(vk.verify(&unsigned, &sig).is_ok())
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.ts) > self.ttl as i64
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- agent card

/// Signed capability descriptor (spec §7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    pub description: String,
    pub endpoint: Aid,
    pub capabilities: Vec<Capability>,
    pub auth_scheme: String, // "ed25519" | "none"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// Ed25519 public key (32 bytes) when auth_scheme == "ed25519".
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_serde_bytes")]
    pub public_key: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_hint: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<i64>,
    /// Signature of the card by its own key (covers all fields above).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_serde_bytes")]
    pub sig: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capability {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<String>>,
}

impl AgentCard {
    pub fn encode_unsigned(&self) -> Result<Vec<u8>, AclError> {
        let mut copy = self.clone();
        copy.sig = None;
        rmp_serde::to_vec_named(&copy).map_err(AclError::Encode)
    }

    pub fn sign(&mut self, key: &SigningKey) -> Result<(), AclError> {
        self.public_key = Some(key.verifying_key().to_bytes().to_vec());
        let unsigned = self.encode_unsigned()?;
        self.sig = Some(key.sign(&unsigned).to_bytes().to_vec());
        Ok(())
    }

    pub fn verify(&self) -> Result<bool, AclError> {
        let (Some(sig_bytes), Some(pk)) = (self.sig.clone(), self.public_key.clone()) else {
            return Ok(false);
        };
        let vk = VerifyingKey::from_bytes(
            <&[u8; 32]>::try_from(&pk[..]).map_err(|_| AclError::BadKey)?,
        )
        .map_err(|_| AclError::BadKey)?;
        let sig = Signature::from_bytes(
            <&[u8; 64]>::try_from(&sig_bytes[..]).map_err(|_| AclError::BadKey)?,
        );
        Ok(vk.verify(&self.encode_unsigned()?, &sig).is_ok())
    }
}

// ---------------------------------------------------------------- framing

/// Length-prefixed stream framing (spec §3): 4-byte BE length + payload.
/// Reads one framed message; returns Ok(None) on EOF at a frame boundary.
pub fn read_frame<R: Read>(mut r: R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

pub fn write_frame(buf: &mut Vec<u8>, payload: &[u8]) {
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
}

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub enum AclError {
    Encode(rmp_serde::encode::Error),
    Decode(rmp_serde::decode::Error),
    Version(u8),
    BadKey,
}

impl std::fmt::Display for AclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AclError::Encode(e) => write!(f, "encode: {e}"),
            AclError::Decode(e) => write!(f, "decode: {e}"),
            AclError::Version(v) => write!(f, "unsupported ACL version {v}"),
            AclError::BadKey => write!(f, "bad ed25519 key"),
        }
    }
}
impl std::error::Error for AclError {}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    fn key() -> SigningKey {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        SigningKey::from_bytes(&seed)
    }

    fn sample() -> Envelope {
        Envelope::new(
            Aid::local(4211, "a1"),
            Aid::new("did:example:agent-b"),
            Performative::Request,
            Body::Task {
                description: "summarize report".into(),
                input: serde_json::json!({"file": "q3.pdf", "lang": "pt"}),
                deadline_ms: Some(60_000),
                priority: Some(5),
            },
        )
    }

    #[test]
    fn roundtrip_all_performatives() {
        let bodies = vec![
            (Performative::Request, Body::Task { description: "t".into(), input: serde_json::json!({"x": 1}), deadline_ms: None, priority: None }),
            (Performative::Inform, Body::Result { output: serde_json::json!("done"), artifacts: vec![Artifact { name: "a.txt".into(), mime: "text/plain".into(), bytes: b"hi".to_vec() }] }),
            (Performative::Inform, Body::Text { text: "hello".into() }),
            (Performative::Query, Body::Query { subject: "agent.card".into(), parameters: None }),
            (Performative::Propose, Body::Proposal { terms: serde_json::json!({"split": "50/50"}), price: Some("2 credits".into()), valid_until: None }),
            (Performative::Accept, Body::Ref { target_msg_id: "abc".into() }),
            (Performative::Reject, Body::Ref { target_msg_id: "abc".into() }),
            (Performative::Cancel, Body::Ref { target_msg_id: "abc".into() }),
            (Performative::Progress, Body::Progress { target_msg_id: "abc".into(), percent: Some(50), note: Some("halfway".into()) }),
            (Performative::InputRequired, Body::InputNeeded { target_msg_id: "abc".into(), what: "missing column".into(), schema: None }),
            (Performative::Failure, Body::Failure { target_msg_id: "abc".into(), code: 500, reason: "no data".into(), recoverable: false }),
            (Performative::Error, Body::Error { code: 400, message: "bad".into(), details: None }),
            (Performative::Hello, Body::Hello { agent_card: None, card_hash: None, nonce: vec![7u8; 16] }),
            (Performative::Bye, Body::Ref { target_msg_id: "abc".into() }),
            (Performative::Delegate, Body::Delegate { task: Box::new(Body::Task { description: "sub".into(), input: serde_json::json!(null), deadline_ms: None, priority: None }), chain: vec!["did:example:a".into(), "did:example:b".into()] }),
        ];
        for (perf, body) in bodies {
            let env = Envelope::new(Aid::local(1, "s"), Aid::new("did:x:y"), perf, body);
            let bytes = env.encode().expect("encode");
            let back = Envelope::decode(&bytes).expect("decode");
            assert_eq!(back.performative, perf);
            assert_eq!(back.msg_id, env.msg_id);
        }
    }

    #[test]
    fn sign_verify() {
        let key = key();
        let mut env = sample();
        env.sign(&key).unwrap();
        assert!(env.verify(&key.verifying_key()).unwrap());
        // tamper: change field → verify must fail
        env.ttl = 1;
        assert!(!env.verify(&key.verifying_key()).unwrap());
    }

    #[test]
    fn wrong_key_fails() {
        let mut env = sample();
        env.sign(&key()).unwrap();
        assert!(!env.verify(&key().verifying_key()).unwrap());
    }

    #[test]
    fn framing_roundtrip() {
        let env = sample();
        let payload = env.encode().unwrap();
        let mut stream = Vec::new();
        write_frame(&mut stream, &payload);
        write_frame(&mut stream, &payload);
        let mut cursor = &stream[..];
        let f1 = read_frame(&mut cursor).unwrap().unwrap();
        let f2 = read_frame(&mut cursor).unwrap().unwrap();
        let eof = read_frame(&mut cursor).unwrap();
        assert_eq!(f1, payload);
        assert_eq!(f2, payload);
        assert!(eof.is_none());
    }

    #[test]
    fn agent_card_sign_verify() {
        let key = key();
        let mut card = AgentCard {
            name: "argus".into(),
            version: "1.0".into(),
            description: "watchdog agent".into(),
            endpoint: Aid::local(4211, "a1"),
            capabilities: vec![Capability {
                name: "log.watch".into(),
                input_schema: None,
                output_schema: None,
                examples: None,
            }],
            auth_scheme: "ed25519".into(),
            key_id: None,
            public_key: None,
            rate_hint: Some(60),
            valid_until: None,
            sig: None,
        };
        card.sign(&key).unwrap();
        assert!(card.verify().unwrap());
        card.name = "tampered".into();
        assert!(!card.verify().unwrap());
    }

    #[test]
    fn compactness() {
        let env = sample();
        let mp = env.encode().unwrap();
        let js = serde_json::to_vec(&env).unwrap();
        println!("msgpack {} bytes vs json {} bytes", mp.len(), js.len());
        assert!(mp.len() < js.len());
    }
}
