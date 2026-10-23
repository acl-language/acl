//! acl-ts — Node.js bindings for acl-core via napi-rs.
//!
//! Exposes: envelope build/sign/verify/encode/decode, framing, msg_id.
//! Compact surface: same semantics as acl-py.

use acl_core::{Aid, Body, Envelope, Performative, ACL_VERSION};
use napi::bindgen_prelude::*;
use napi_derive::napi;

fn to_js_err(e: acl_core::AclError) -> Error {
    Error::new(Status::GenericFailure, e.to_string())
}

/// Native Ed25519 key.
#[napi]
pub struct AclKey {
    inner: ed25519_dalek::SigningKey,
}

#[napi]
impl AclKey {
    #[napi(constructor)]
    pub fn new() -> Self {
        use rand::Rng;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        AclKey {
            inner: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    #[napi(factory)]
    pub fn from_seed(seed: Buffer) -> Result<AclKey> {
        if seed.len() != 32 {
            return Err(Error::new(Status::InvalidArg, "seed must be 32 bytes"));
        }
        Ok(AclKey {
            inner: ed25519_dalek::SigningKey::from_bytes(
                &<[u8; 32]>::try_from(seed.as_ref()).unwrap(),
            ),
        })
    }

    #[napi]
    pub fn public_bytes(&self) -> Buffer {
        self.inner.verifying_key().to_bytes().to_vec().into()
    }
}

/// The ACL envelope (SPEC.md §4). `body` is a JSON string: {"t": kind, "v": {...}}.
#[napi]
pub struct AclEnvelope {
    inner: acl_core::Envelope,
}

#[napi]
impl AclEnvelope {
    #[napi(constructor)]
    pub fn new(
        from_aid: String,
        to_aid: String,
        performative: String,
        body_json: String,
        in_reply_to: Option<String>,
        reply_with: Option<String>,
    ) -> Result<AclEnvelope> {
        let body: Body = serde_json::from_str::<serde_json::Value>(&body_json)
            .ok()
            .and_then(|v| json_to_body(&v))
            .ok_or_else(|| Error::new(Status::InvalidArg, "invalid body"))?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Ok(AclEnvelope {
            inner: Envelope {
                acl: ACL_VERSION,
                msg_id: acl_core::new_msg_id(),
                in_reply_to,
                from: Aid::new(from_aid),
                to: Aid::new(to_aid),
                ts,
                ttl: acl_core::DEFAULT_TTL_MS,
                reply_with,
                hop_count: 0,
                performative: perf_from_str(&performative)?,
                body,
                sig: None,
                rational_effect: None,
                feasibility_precondition: None,
            },
        })
    }

    #[napi]
    pub fn sign(&mut self, key: &AclKey) -> Result<()> {
        self.inner.sign(&key.inner).map_err(to_js_err)
    }

    #[napi]
    pub fn verify(&self, pubkey: Buffer) -> Result<bool> {
        let vk = ed25519_dalek::VerifyingKey::from_bytes(
            &<[u8; 32]>::try_from(pubkey.as_ref())
                .map_err(|_| Error::new(Status::InvalidArg, "pubkey must be 32 bytes"))?,
        )
        .map_err(|_| Error::new(Status::InvalidArg, "bad key"))?;
        Ok(self.inner.verify(&vk).map_err(to_js_err)?)
    }

    #[napi]
    pub fn encode(&self) -> Result<Buffer> {
        Ok(self.inner.encode().map_err(to_js_err)?.into())
    }

    #[napi(factory)]
    pub fn decode(data: Buffer) -> Result<AclEnvelope> {
        Ok(AclEnvelope {
            inner: Envelope::decode(data.as_ref()).map_err(to_js_err)?,
        })
    }

    #[napi(getter)]
    pub fn msg_id(&self) -> String {
        self.inner.msg_id.clone()
    }
    #[napi(getter)]
    pub fn performative(&self) -> String {
        perf_name(self.inner.performative).to_string()
    }
    #[napi(getter)]
    pub fn in_reply_to(&self) -> Option<String> {
        self.inner.in_reply_to.clone()
    }
    #[napi(getter)]
    pub fn body_json(&self) -> String {
        serde_json::to_string(&body_to_json(&self.inner.body)).unwrap_or_default()
    }

    #[napi]
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.inner.is_expired(now)
    }
}

/// Frame: 4-byte BE length + payload (spec §3).
#[napi]
pub fn write_frame(payload: Buffer) -> Buffer {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload.as_ref());
    out.into()
}

/// Unframe; returns null when fewer than 4 bytes.
#[napi]
pub fn read_frame(stream: Buffer) -> Option<Buffer> {
    let b = stream.as_ref();
    if b.len() < 4 {
        return None;
    }
    let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    if b.len() < 4 + n {
        return None;
    }
    Some(b[4..4 + n].to_vec().into())
}

#[napi]
pub fn new_msg_id() -> String {
    acl_core::new_msg_id()
}

// ---- JSON <-> Body (mirror of acl-py's rule: {"t": kind, "v": {...}}) -----

fn perf_from_str(name: &str) -> Result<Performative> {
    Ok(match name {
        "request" => Performative::Request,
        "inform" => Performative::Inform,
        "query" => Performative::Query,
        "propose" => Performative::Propose,
        "accept" => Performative::Accept,
        "reject" => Performative::Reject,
        "cancel" => Performative::Cancel,
        "progress" => Performative::Progress,
        "input_required" => Performative::InputRequired,
        "failure" => Performative::Failure,
        "delegate" => Performative::Delegate,
        "error" => Performative::Error,
        "hello" => Performative::Hello,
        "bye" => Performative::Bye,
        _ => return Err(Error::new(Status::InvalidArg, format!("unknown performative {name}"))),
    })
}

fn perf_name(p: Performative) -> &'static str {
    match p {
        Performative::Request => "request",
        Performative::Inform => "inform",
        Performative::Query => "query",
        Performative::Propose => "propose",
        Performative::Accept => "accept",
        Performative::Reject => "reject",
        Performative::Cancel => "cancel",
        Performative::Progress => "progress",
        Performative::InputRequired => "input_required",
        Performative::Failure => "failure",
        Performative::Delegate => "delegate",
        Performative::Error => "error",
        Performative::Hello => "hello",
        Performative::Bye => "bye",
    }
}

fn json_to_body(v: &serde_json::Value) -> Option<Body> {
    let kind = v.get("t")?.as_str()?;
    let f = v.get("v")?;
    let s = |n: &str| f.get(n).and_then(|x| x.as_str()).map(String::from);
    let opt_num = |n: &str| f.get(n).and_then(|x| x.as_u64());
    Some(match kind {
        "Task" => Body::Task {
            description: s("description")?,
            input: f.get("input").cloned().unwrap_or(serde_json::Value::Null),
            deadline_ms: opt_num("deadline_ms").map(|x| x as u32),
            priority: opt_num("priority").map(|x| x as u8),
        },
        "Result" => Body::Result {
            output: f.get("output").cloned().unwrap_or(serde_json::Value::Null),
            artifacts: vec![],
        },
        "Text" => Body::Text { text: s("text")? },
        "Query" => Body::Query {
            subject: s("subject")?,
            parameters: f.get("parameters").cloned(),
        },
        "Proposal" => Body::Proposal {
            terms: f.get("terms").cloned().unwrap_or(serde_json::Value::Null),
            price: s("price"),
            valid_until: f.get("valid_until").and_then(|x| x.as_i64()),
        },
        "Ref" => Body::Ref { target_msg_id: s("target_msg_id")? },
        "Progress" => Body::Progress {
            target_msg_id: s("target_msg_id")?,
            percent: opt_num("percent").map(|x| x as u8),
            note: s("note"),
        },
        "InputNeeded" => Body::InputNeeded {
            target_msg_id: s("target_msg_id")?,
            what: s("what")?,
            schema: f.get("schema").cloned(),
        },
        "Failure" => Body::Failure {
            target_msg_id: s("target_msg_id")?,
            code: opt_num("code").map(|x| x as u16).unwrap_or(500),
            reason: s("reason")?,
            recoverable: f.get("recoverable").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        "Error" => Body::Error {
            code: opt_num("code").map(|x| x as u16).unwrap_or(400),
            message: s("message")?,
            details: f.get("details").cloned(),
        },
        "Hello" => Body::Hello {
            agent_card: None,
            card_hash: None,
            nonce: vec![],
        },
        "Sealed" => Body::Sealed {
            sender_pub: vec![],
            salt: vec![],
            ciphertext: vec![],
        },
        "Cl" => {
            let exprs: Vec<acl_core::cl::ClExpr> = serde_json::from_value(
                f.get("expressions").cloned().unwrap_or(serde_json::Value::Null),
            )
            .ok()?;
            Body::Cl { expressions: exprs }
        }
        "Delegate" => Body::Delegate {
            task: Box::new(Body::Task {
                description: "nested".into(),
                input: serde_json::Value::Null,
                deadline_ms: None,
                priority: None,
            }),
            chain: vec![],
        },
        _ => return None,
    })
}

fn body_to_json(b: &Body) -> serde_json::Value {
    let (t, v) = match b {
        Body::Task { description, input, deadline_ms, priority } => ("Task", serde_json::json!({
            "description": description, "input": input,
            "deadline_ms": deadline_ms, "priority": priority })),
        Body::Result { output, .. } => ("Result", serde_json::json!({ "output": output, "artifacts": [] })),
        Body::Text { text } => ("Text", serde_json::json!({ "text": text })),
        Body::Query { subject, parameters } => ("Query", serde_json::json!({ "subject": subject, "parameters": parameters })),
        Body::Proposal { terms, price, valid_until } => ("Proposal", serde_json::json!({ "terms": terms, "price": price, "valid_until": valid_until })),
        Body::Ref { target_msg_id } => ("Ref", serde_json::json!({ "target_msg_id": target_msg_id })),
        Body::Progress { target_msg_id, percent, note } => ("Progress", serde_json::json!({ "target_msg_id": target_msg_id, "percent": percent, "note": note })),
        Body::InputNeeded { target_msg_id, what, schema } => ("InputNeeded", serde_json::json!({ "target_msg_id": target_msg_id, "what": what, "schema": schema })),
        Body::Failure { target_msg_id, code, reason, recoverable } => ("Failure", serde_json::json!({ "target_msg_id": target_msg_id, "code": code, "reason": reason, "recoverable": recoverable })),
        Body::Error { code, message, details } => ("Error", serde_json::json!({ "code": code, "message": message, "details": details })),
        Body::Hello { nonce, .. } => ("Hello", serde_json::json!({ "nonce": nonce })),
        Body::Sealed { .. } => ("Sealed", serde_json::json!({})),
        Body::Cl { expressions } => {
            let v = serde_json::to_value(expressions).unwrap_or(serde_json::Value::Null);
            ("Cl", serde_json::json!({ "expressions": v }))
        }
        Body::Delegate { chain, .. } => ("Delegate", serde_json::json!({ "chain": chain })),
    };
    serde_json::json!({ "t": t, "v": v })
}
