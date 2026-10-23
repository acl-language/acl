//! acl-playground — generates the browser playground transcript (§13).
//!
//! Runs a REAL conversation between two in-process agents (argus the
//! coordinator, kenji the worker): real Ed25519 keypairs, real did:key
//! AIDs, real signatures verified by the receiver, real wire bytes.
//! Emits `transcript.json` for docs/playground/index.html to animate.
//!
//! Usage: acl-playground <out.json>

use acl_core::cl::ClExpr;
use acl_core::did;
use acl_core::{Aid, Body, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use serde::Serialize;

#[derive(Serialize)]
struct AgentInfo {
    name: &'static str,
    role: &'static str,
    did_key: String,
    pubkey_hex: String,
}

#[derive(Serialize)]
struct Msg {
    seq: usize,
    from: String,
    to: String,
    performative: String,
    size_bytes: usize,
    /// full msgpack envelope (with sig), hex — exactly what was "on the wire"
    wire_hex: String,
    /// decoded envelope (sig field stripped for readability)
    decoded: serde_json::Value,
    /// receiver verified the sender's signature?
    sig_verified: bool,
    note: &'static str,
}

#[derive(Serialize)]
struct Transcript {
    protocol: &'static str,
    version: u8,
    agents: Vec<AgentInfo>,
    messages: Vec<Msg>,
}

struct Agent {
    name: &'static str,
    role: &'static str,
    key: SigningKey,
    aid: Aid,
}

impl Agent {
    fn new(name: &'static str, role: &'static str) -> Self {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let key = SigningKey::from_bytes(&seed);
        let aid = Aid::new(&did::verifying_key_to_did_key(&key.verifying_key()));
        Agent { name, role, key, aid }
    }

    fn info(&self) -> AgentInfo {
        AgentInfo {
            name: self.name,
            role: self.role,
            did_key: self.aid.0.clone(),
            pubkey_hex: hex(&self.key.verifying_key().to_bytes()),
        }
    }

    fn send(&self, to: &Aid, perf: Performative, body: Body, in_reply_to: Option<String>) -> Envelope {
        let mut env = Envelope::new(self.aid.clone(), to.clone(), perf, body);
        env.in_reply_to = in_reply_to; // must be set BEFORE signing (§10)
        env.sign(&self.key).expect("sign");
        env
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Receiver-side verification + transcript record.
fn record(
    seq: usize,
    env: &Envelope,
    receiver_vk: &VerifyingKey,
    note: &'static str,
) -> Msg {
    let decoded = serde_json::to_value(env).unwrap_or(serde_json::Value::Null);
    Msg {
        seq,
        from: env.from.0.clone(),
        to: env.to.0.clone(),
        performative: format!("{:?}", env.performative),
        size_bytes: env.encode().map(|b| b.len()).unwrap_or(0),
        wire_hex: hex(&env.encode().unwrap_or_default()),
        decoded,
        sig_verified: env.verify(receiver_vk).unwrap_or(false),
        note,
    }
}

fn stripped(env: &Envelope) -> serde_json::Value {
    let mut v = serde_json::to_value(env).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("sig");
    }
    v
}

/// variant of record with a cleaner decoded view (sig removed)
macro_rules! rec {
    ($seq:expr, $env:expr, $vk:expr, $note:expr) => {{
        let mut m = record($seq, &$env, $vk, $note);
        m.decoded = stripped(&$env);
        m
    }};
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args.get(1).cloned().unwrap_or_else(|| "transcript.json".into());

    let argus = Agent::new("argus", "coordinator");
    let kenji = Agent::new("kenji", "worker");
    let agents = vec![argus.info(), kenji.info()];
    let mut msgs: Vec<Msg> = Vec::new();
    let mut seq = 0usize;

    // ---- 1. task with input → immediate result
    seq += 1;
    let m1 = argus.send(
        &kenji.aid,
        Performative::Request,
        Body::Task {
            description: "reverse this text".into(),
            input: serde_json::json!({"text": "hello agents"}),
            deadline_ms: Some(60_000),
            priority: Some(5),
        },
        None,
    );
    let vk = argus.key.verifying_key();
    msgs.push(rec!(seq, m1, &vk, "coordinator posts a task (request/Task)"));

    seq += 1;
    let m2 = kenji.send(
        &argus.aid,
        Performative::Inform,
        Body::Result {
            output: serde_json::json!({"reversed": "stnega olleh"}),
            artifacts: vec![],
        },
        Some(m1.msg_id.clone()),
    );
    let vk = kenji.key.verifying_key();
    msgs.push(rec!(seq, m2, &vk, "worker answers (inform/Result)"));

    // ---- 2. task missing input → input_required → resume
    seq += 1;
    let m3 = argus.send(
        &kenji.aid,
        Performative::Request,
        Body::Task {
            description: "code a feature".into(),
            input: serde_json::json!({}),
            deadline_ms: None,
            priority: None,
        },
        None,
    );
    let vk = argus.key.verifying_key();
    msgs.push(rec!(seq, m3, &vk, "task without required input"));

    seq += 1;
    let m4 = kenji.send(
        &argus.aid,
        Performative::InputRequired,
        Body::InputNeeded {
            target_msg_id: m3.msg_id.clone(),
            what: "feature description (input.text)".into(),
            schema: Some(serde_json::json!({
                "type": "object", "required": ["text"],
                "properties": {"text": {"type": "string"}}
            })),
        },
        Some(m3.msg_id.clone()),
    );
    let vk = kenji.key.verifying_key();
    msgs.push(rec!(seq, m4, &vk, "worker blocks: input_required (§8)"));

    seq += 1;
    let m5 = argus.send(
        &kenji.aid,
        Performative::Inform,
        Body::Text {
            text: "add a /health endpoint".into(),
        },
        Some(m4.msg_id.clone()),
    );
    let vk = argus.key.verifying_key();
    msgs.push(rec!(seq, m5, &vk, "coordinator supplies the missing input"));

    seq += 1;
    let m6 = kenji.send(
        &argus.aid,
        Performative::Inform,
        Body::Result {
            output: serde_json::json!({"coded": "add a /health endpoint", "lines": 1}),
            artifacts: vec![],
        },
        Some(m5.msg_id.clone()),
    );
    let vk = kenji.key.verifying_key();
    msgs.push(rec!(seq, m6, &vk, "work resumed and completed"));

    // ---- 3. content language: fact query → fact
    seq += 1;
    let m7 = argus.send(
        &kenji.aid,
        Performative::Query,
        Body::Cl {
            expressions: vec![ClExpr::query(
                "file_exists",
                serde_json::json!({"path": "/etc/hostname"}),
            )],
        },
        None,
    );
    let vk = argus.key.verifying_key();
    msgs.push(rec!(seq, m7, &vk, "content language: fact query (§6.2)"));

    seq += 1;
    let exists = std::path::Path::new("/etc/hostname").exists();
    let m8 = kenji.send(
        &argus.aid,
        Performative::Inform,
        Body::Cl {
            expressions: vec![{
                let mut f = ClExpr::fact("file_exists", serde_json::json!({"path": "/etc/hostname"}));
                if let ClExpr::Fact { truth, .. } = &mut f {
                    *truth = exists;
                }
                f
            }],
        },
        Some(m7.msg_id.clone()),
    );
    let vk = kenji.key.verifying_key(); // sender signs → sender's key verifies
    msgs.push(rec!(seq, m8, &vk, "content language: fact answer"));

    let all_verified = msgs.iter().all(|m| m.sig_verified);
    eprintln!(
        "[acl-playground] {} messages, all signatures verified: {}",
        msgs.len(),
        all_verified
    );
    assert!(all_verified, "every message must verify");

    let transcript = Transcript {
        protocol: "ACL",
        version: acl_core::ACL_VERSION,
        agents,
        messages: msgs,
    };
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&transcript).expect("serialize"),
    )
    .expect("write");
    eprintln!("[acl-playground] wrote {out_path}");
}
