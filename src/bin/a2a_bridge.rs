//! acl-a2a-bridge — ACL on one side, real A2A over HTTP on the other.
//!
//! Serves ACL on stdio; forwards request/Task as A2A `message/send` to an
//! A2A server URL; maps the returned Task states via bridge.rs. Also
//! fetches the A2A Agent Card for `query agent.card`.

use acl_core::bridge::{a2a_state_to_acl, acl_to_a2a_state, A2aState};
use acl_core::{AgentCard, Aid, Body, Capability, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use std::io::{BufRead, BufReader, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .iter()
        .position(|a| a == "--a2a")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:9400".into());
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let my_aid = Aid::local(std::process::id(), "a2a-bridge");
    eprintln!("[a2a-bridge] target={url}");

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = std::io::stdout();
    while let Ok(Some(payload)) = acl_core::read_frame(&mut reader) {
        let env = match Envelope::decode(&payload) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[a2a-bridge] decode: {e}");
                continue;
            }
        };
        eprintln!("[a2a-bridge] <- {:?}", env.performative);

        let (perf, body) = match (&env.performative, &env.body) {
            (Performative::Request, Body::Task { input, .. }) => {
                let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
                // A2A message/send (JSON-RPC over HTTP POST)
                let req = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "message/send",
                    "params": {"message": {
                        "role": "user",
                        "parts": [{"kind": "text", "text": text}],
                        "taskId": env.msg_id,
                    }}
                });
                let resp = http_post(&format!("{url}/"), &req.to_string());
                let parsed: serde_json::Value = match serde_json::from_str(&resp) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[a2a-bridge] a2a parse: {e}: {resp}");
                        let err = Body::Error {
 code: 502, message: "a2a unreachable".into(), taxonomy: None, details: None };
                        let mut out = Envelope::new(my_aid.clone(), env.from.clone(), Performative::Error, err);
                        out.in_reply_to = Some(env.msg_id.clone());
                        out.sign(&key).expect("sign");
                        send_frame(&mut stdout, &out);
                        continue;
                    }
                };
                let task = parsed["result"].clone();
                let state = task["status"]["state"].as_str().unwrap_or("UNKNOWN");
                eprintln!("[a2a-bridge] a2a state={state}");
                let acl_perf = a2a_state_to_acl(state)
                    .unwrap_or(Performative::Failure);
                let reversed = task["artifacts"][0]["parts"][0]["data"]["reversed"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (acl_perf, Body::Result {
                    output: serde_json::json!({"reversed": reversed, "via": "a2a", "task": task["id"]}),
                    artifacts: vec![],
                })
            }
            (Performative::Query, Body::Query { subject, .. }) if subject == "agent.card" => {
                // fetch A2A Agent Card and re-shape as ACL AgentCard
                let card_json = http_get(&format!("{url}/.well-known/agent-card.json"));
                let card: serde_json::Value = serde_json::from_str(&card_json)
                    .unwrap_or(serde_json::json!({}));
                let skills = card["skills"].as_array().cloned().unwrap_or_default();
                (
                    Performative::Hello,
                    Body::Hello {
                        agent_card: Some(AgentCard {
                            name: card["name"].as_str().unwrap_or("a2a-peer").into(),
                            version: card["protocolVersion"].as_str().unwrap_or("1.0").into(),
                            description: card["description"].as_str().unwrap_or_default().into(),
                            endpoint: Aid::new(&url),
                            capabilities: skills
                                .iter()
                                .map(|s| Capability {
                                    name: s["id"].as_str().unwrap_or_default().into(),
                                    input_schema: None,
                                    output_schema: None,
                                    examples: None,
                                })
                                .collect(),
                            auth_scheme: "none".into(),
                            key_id: None,
                            public_key: None,
                            rate_hint: None,
                            valid_until: None,
                            sig: None,
                        }),
                        card_hash: None,
                        nonce: vec![],
                    },
                )
            }
            _ => (
                Performative::Error,
                Body::Error {
 code: 400, message: "bridge supports request/Task and query/agent.card".into(), taxonomy: None, details: None },
            ),
        };

        let mut out = Envelope::new(my_aid.clone(), env.from.clone(), perf, body);
        out.in_reply_to = Some(env.msg_id.clone());
        out.sign(&key).expect("sign");
        send_frame(&mut stdout, &out);
        eprintln!("[a2a-bridge] -> {:?}", out.performative);
    }
    // keep bridge helpers referenced
    let _ = acl_to_a2a_state;
    let _ = A2aState::Submitted;
}

trait ReplyHelper {
    fn into_reply(&self, env: &Envelope, key: &SigningKey, my_aid: &Aid, out: &mut dyn Write);
}
impl ReplyHelper for Body {
    fn into_reply(&self, env: &Envelope, key: &SigningKey, my_aid: &Aid, out: &mut dyn Write) {
        let mut e = Envelope::new(my_aid.clone(), env.from.clone(), Performative::Error, self.clone());
        e.in_reply_to = Some(env.msg_id.clone());
        e.sign(key).expect("sign");
        send_frame(out, &e);
    }
}

fn send_frame(out: &mut dyn Write, env: &Envelope) {
    let encoded = env.encode().expect("encode");
    let mut framed = Vec::with_capacity(encoded.len() + 4);
    framed.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    framed.extend_from_slice(&encoded);
    out.write_all(&framed).expect("write");
    out.flush().expect("flush");
}

fn http_post(url: &str, body: &str) -> String {
    let (host, port, path) = split_url(url);
    let mut s = std::net::TcpStream::connect((host.as_str(), port)).expect("connect");
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    read_http_body(&mut s)
}

fn http_get(url: &str) -> String {
    let (host, port, path) = split_url(url);
    let mut s = std::net::TcpStream::connect((host.as_str(), port)).expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    read_http_body(&mut s)
}

fn split_url(url: &str) -> (String, u16, String) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (hostport, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(80), path),
        None => (hostport.to_string(), 80, path),
    }
}

fn read_http_body(s: &mut std::net::TcpStream) -> String {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&raw);
    let Some(i) = text.find("\r\n\r\n") else { return text.to_string(); };
    let headers = &text[..i];
    let body = &text[i + 4..];
    if headers.to_lowercase().contains("transfer-encoding: chunked") {
        dechunk(body)
    } else {
        body.to_string()
    }
}

/// Minimal chunked decoding: read "<hex>\r\n", that many bytes, repeat to 0.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    loop {
        let Some(i) = rest.find("\r\n") else { break };
        let size = usize::from_str_radix(rest[..i].trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = i + 2;
        if rest.len() < start + size {
            out.push_str(&rest[start..]);
            break;
        }
        out.push_str(&rest[start..start + size]);
        rest = &rest[start + size..];
        if rest.starts_with("\r\n") {
            rest = &rest[2..];
        }
    }
    out
}
