//! acl-peer — an ACL agent speaking the stdio transport (SPEC.md §9).
//!
//! Reads length-framed envelopes from stdin, writes framed replies to
//! stdout. Diagnostics go to stderr only. Demonstrates the §8 workflow:
//! request → (input_required → inform) → result, plus agent.card query.
//!
//! Usage: acl-peer --name NAME --pub-out FILE [--key FILE]

use acl_core::{
    AgentCard, Aid, Body, Capability, Envelope, Performative, ACL_VERSION,
};
use ed25519_dalek::SigningKey;
use rand::Rng;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

fn mk(my_aid: &Aid, req: &Envelope, perf: Performative, body: Body) -> Envelope {
    let mut out = Envelope::new(my_aid.clone(), req.from.clone(), perf, body);
    out.in_reply_to = Some(req.msg_id.clone());
    out.reply_with = req.reply_with.clone();
    out
}

fn main() {
    let mut name = String::from("peer");
    let mut key_file = PathBuf::from("/tmp/acl-peer.key");
    let mut pub_out = String::new();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = args[i].clone();
            }
            "--key" => {
                i += 1;
                key_file = PathBuf::from(&args[i]);
            }
            "--pub-out" => {
                i += 1;
                pub_out = args[i].clone();
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // load or create the signing key
    let key = match std::fs::read(&key_file) {
        Ok(bytes) if bytes.len() == 32 => SigningKey::from_bytes(
            &<[u8; 32]>::try_from(bytes.as_slice()).expect("32 bytes"),
        ),
        _ => {
            let mut seed = [0u8; 32];
            rand::rng().fill_bytes(&mut seed);
            std::fs::write(&key_file, &seed).expect("write key");
            SigningKey::from_bytes(&seed)
        }
    };
    if !pub_out.is_empty() {
        std::fs::write(&pub_out, key.verifying_key().to_bytes()).expect("write pub");
    }
    let my_aid = Aid::local(std::process::id(), &name);
    eprintln!("[{name}] aid={} acl={ACL_VERSION} ready", my_aid.0);

    let mut blocked: HashMap<String, (String, String)> = HashMap::new(); // reply msg_id -> (task msg_id, description)

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = std::io::stdout();

    while let Ok(Some(payload)) = acl_core::read_frame(&mut reader) {
        let env = match Envelope::decode(&payload) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[{name}] decode error: {e}");
                continue;
            }
        };
        eprintln!(
            "[{name}] <- {:?} from={} msg={}",
            env.performative, env.from.0, env.msg_id
        );

        // Each arm builds its full reply envelope (so we can track the
        // reply's own msg_id — the client answers referencing IT, per §8).
        let reply = match (&env.performative, &env.body) {
            (Performative::Request, Body::Task { description, input, .. }) => {
                let text = input.get("text").and_then(|v| v.as_str());
                match text {
                    Some(t) => {
                        let reversed: String = t.chars().rev().collect();
                        Some(mk(
                            &my_aid, &env, Performative::Inform,
                            Body::Result {
                                output: serde_json::json!({"reversed": reversed}),
                                artifacts: vec![],
                            },
                        ))
                    }
                    None => {
                        let mut out = mk(
                            &my_aid, &env, Performative::InputRequired,
                            Body::InputNeeded {
                                target_msg_id: env.msg_id.clone(),
                                what: "input.text (string to reverse)".into(),
                                schema: Some(serde_json::json!({
                                    "type": "object",
                                    "required": ["text"],
                                    "properties": {"text": {"type": "string"}}
                                })),
                            },
                        );
                        // §8: the client's inform will reference THIS msg_id
                        blocked.insert(
                            out.msg_id.clone(),
                            (env.msg_id.clone(), description.clone()),
                        );
                        Some(out)
                    }
                }
            }
            (Performative::Inform, _) => {
                if let Some((task_id, desc)) = env
                    .in_reply_to
                    .as_ref()
                    .and_then(|id| blocked.remove(id))
                {
                    let _ = task_id;
                    Some(mk(
                        &my_aid, &env, Performative::Inform,
                        Body::Result {
                            output: serde_json::json!({
                                "completed": desc, "note": "resumed after input_required"
                            }),
                            artifacts: vec![],
                        },
                    ))
                } else {
                    Some(mk(
                        &my_aid, &env, Performative::Inform,
                        Body::Text { text: "ack".into() },
                    ))
                }
            }
            (Performative::Query, Body::Query { subject, .. })
                if subject == "agent.card" =>
            {
                let mut card = AgentCard {
                    name: name.clone(),
                    version: "0.1".into(),
                    description: "stdio demo peer".into(),
                    endpoint: my_aid.clone(),
                    capabilities: vec![Capability {
                        name: "text.reverse".into(),
                        input_schema: Some(serde_json::json!({
                            "type": "object", "required": ["text"]
                        })),
                        output_schema: None,
                        examples: Some(vec!["\"abc\" -> \"cba\"".into()]),
                    }],
                    auth_scheme: "ed25519".into(),
                    key_id: None,
                    public_key: None,
                    rate_hint: None,
                    valid_until: None,
                    sig: None,
                };
                card.sign(&key).expect("card sign");
                Some(mk(
                    &my_aid, &env, Performative::Hello,
                    Body::Hello {
                        agent_card: Some(card),
                        card_hash: None,
                        nonce: vec![],
                    },
                ))
            }
            (Performative::Bye, _) => {
                eprintln!("[{name}] session closed by peer");
                std::process::exit(0);
            }
            _ => Some(mk(
                &my_aid, &env, Performative::Error,
                Body::Error {
                    code: 400,
                    message: format!("unexpected {:?}", env.performative),
                    details: None,
                },
            )),
        };

        if let Some(mut out) = reply {
            out.sign(&key).expect("sign");
            let encoded = out.encode().expect("encode");
            let mut framed = Vec::with_capacity(encoded.len() + 4);
            framed.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
            framed.extend_from_slice(&encoded);
            stdout.write_all(&framed).expect("write");
            stdout.flush().expect("flush");
            eprintln!("[{name}] -> {:?}", out.performative);
        }
    }
}

