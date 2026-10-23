//! acl-peer — an ACL agent speaking the stdio transport (SPEC.md §9).
//!
//! Reads length-framed envelopes from stdin, writes framed replies to
//! stdout. Diagnostics go to stderr only. Demonstrates the §8 workflow:
//! request → (input_required → inform) → result, plus agent.card query.
//!
//! Usage: acl-peer --name NAME --pub-out FILE [--key FILE]

use acl_core::{
    cl::{well_formed, ClExpr},
    schema::RuntimeSchema,
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
    let mut schema_path: Option<String> = None;
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
            "--schema" => {
                i += 1;
                schema_path = Some(args[i].clone());
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
    // §19: runtime schema validation — Cl bodies checked against the
    // compiled vocabulary BEFORE processing
    let schema: Option<RuntimeSchema> = schema_path.as_ref().and_then(|p| {
        match std::fs::read(p).map_err(|e| e.to_string()).and_then(|b| RuntimeSchema::from_json(&b)) {
            Ok(s) => {
                eprintln!("[{name}] schema loaded: {} v{} ({} predicates, {} actions)",
                    s.namespace, s.version, s.predicates.len(), s.actions.len());
                Some(s)
            }
            Err(e) => {
                eprintln!("[{name}] schema load FAILED: {e}");
                None
            }
        }
    });
    let validate_cl = |exprs: &[ClExpr]| -> Option<String> {
        let schema = schema.as_ref()?;
        match schema.validate_all(exprs) {
            Ok(()) => None,
            Err(violations) => Some(violations.join("; ")),
        }
    };
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
            // §6.2: the peer SPEAKS the content language — answers Cl
            // queries with facts, executes Cl actions, enforces grammar.
            (Performative::Query, Body::Cl { expressions }) => {
                if let Some(v) = validate_cl(expressions) {
                    Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                        code: 422, message: format!("schema violation: {v}"), details: None,
                    }))
                } else if let Err(e) = well_formed(env.performative, expressions) {
                    Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                        code: 422, message: format!("ill-formed CL: {e}"), details: None,
                    }))
                } else {
                    let mut facts: Vec<ClExpr> = Vec::new();
                    for e in expressions {
                        if let ClExpr::Query { predicate, args, .. } = e {
                            // accept both bare and namespace-qualified names:
                            // match the local segment (schema vocabularies use
                            // full names; the peer's own vocabulary is local)
                            let local = predicate.rsplit('.').next().unwrap_or(predicate);
                            match local {
                                "file_exists" => {
                                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                    let truth = std::path::Path::new(path).exists();
                                    facts.push(ClExpr::fact("file_exists", serde_json::json!({"path": path})));
                                    if let ClExpr::Fact { truth: t, .. } = facts.last_mut().unwrap() { *t = truth; }
                                }
                                "task_status" => {
                                    let id = args.get("msg_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let truth = blocked.values().any(|(tid, _)| *tid == id);
                                    facts.push(ClExpr::fact("task_status", serde_json::json!({"msg_id": id})));
                                    if let ClExpr::Fact { truth: t, .. } = facts.last_mut().unwrap() { *t = truth; }
                                }
                                other => eprintln!("[{name}] unknown predicate '{other}' (skipped — vocabulary is agent-specific)"),
                            }
                        }
                    }
                    if facts.is_empty() {
                        Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                            code: 404, message: "no known predicate in query".into(), details: None,
                        }))
                    } else {
                        Some(mk(&my_aid, &env, Performative::Inform, Body::Cl { expressions: facts }))
                    }
                }
            }
            (Performative::Request, Body::Cl { expressions }) => {
                if let Some(v) = validate_cl(expressions) {
                    Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                        code: 422, message: format!("schema violation: {v}"), details: None,
                    }))
                } else if let Err(e) = well_formed(env.performative, expressions) {
                    Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                        code: 422, message: format!("ill-formed CL: {e}"), details: None,
                    }))
                } else {
                    let mut results: Vec<ClExpr> = Vec::new();
                    let mut unknown: Option<String> = None;
                    for e in expressions {
                        if let ClExpr::Action { name, params, .. } = e {
                            match name.as_str() {
                                "text.reverse" => {
                                    let t = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                    let rev: String = t.chars().rev().collect();
                                    results.push(ClExpr::fact("text_reversed", serde_json::json!({"input": t, "output": rev})));
                                }
                                other => { unknown = Some(other.to_string()); break; }
                            }
                        }
                    }
                    if let Some(u) = unknown {
                        Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                            code: 400, message: format!("unknown action '{u}'"), details: None,
                        }))
                    } else if results.is_empty() {
                        Some(mk(&my_aid, &env, Performative::Error, Body::Error {
                            code: 400, message: "no actions executed".into(), details: None,
                        }))
                    } else {
                        Some(mk(&my_aid, &env, Performative::Inform, Body::Cl { expressions: results }))
                    }
                }
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

