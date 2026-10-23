//! acl-httpd — ACL over HTTP + SSE transport (SPEC.md §9).
//!
//!   POST /            body = framed ACL envelope  → response body = framed envelope
//!   GET  /events?text=T  text/event-stream: one signed envelope per event
//!                     (progress… then final Result), base64 msgpack payloads
//!   GET  /agent-card  framed Hello envelope carrying the signed AgentCard
//!
//! Minimal HTTP/1.1 over std TCP — no runtime, no framework. Diagnostics on stderr.

use acl_core::{AgentCard, Aid, Body, Capability, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::Rng;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(9200);
    let token: Option<String> = args
        .iter()
        .position(|a| a == "--token")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    std::fs::write("/tmp/acl-httpd.pub", key.verifying_key().to_bytes()).expect("pub");
    match &token {
        Some(_) => eprintln!("[acl-httpd] bearer-token auth ENABLED"),
        None => eprintln!("[acl-httpd] bearer-token auth DISABLED (dev mode)"),
    }

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    eprintln!("[acl-httpd] listening on 127.0.0.1:{port}, acl=1");
    for stream in listener.incoming() {
        if let Ok(s) = stream {
            handle(s, &key, token.as_deref());
        }
    }
}

/// §37: transport auth — check `Authorization: Bearer <token>`
fn authorized(head: &str, token: Option<&str>) -> bool {
    match token {
        None => true, // dev mode: no auth required
        Some(t) => head
            .lines()
            .any(|l| {
                let l = l.trim();
                l.starts_with("authorization:") || l.starts_with("Authorization:")
            } && l.to_lowercase().contains(&format!("bearer {t}"))),
    }
}

fn handle(mut stream: TcpStream, key: &SigningKey, token: Option<&str>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // read until we have full headers, then body per content-length
    let header_end = loop {
        let n = stream.read(&mut chunk).expect("read");
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    for l in lines {
        let mut kv = l.splitn(2, ':');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).expect("read");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = &buf[header_end..header_end + content_length];

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path_q = parts.next().unwrap_or_default().to_string();
    let (path, query) = match path_q.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (path_q.clone(), None),
    };
    eprintln!("[acl-httpd] {method} {path}");

    // §37: bearer-token gate on every ACL-carrying route (agent-card stays
    // open: capability discovery must be verifiable pre-auth)
    if !authorized(&head, token) && path != "/agent-card" {
        let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n                    WWW-Authenticate: Bearer realm=\"acl\"\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let my_aid = Aid::new("http://127.0.0.1/agent/acl-httpd");

    match (method.as_str(), path.as_str()) {
        ("POST", "/") => {
            let payload = strip_frame(body);
            let env = match Envelope::decode(payload) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[acl-httpd] decode error: {e}");
                    return;
                }
            };
            let reply_body = match (&env.performative, &env.body) {
                (Performative::Request, Body::Task { input, .. }) => {
                    let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let reversed: String = text.chars().rev().collect();
                    Body::Result {
                        output: serde_json::json!({"reversed": reversed}),
                        artifacts: vec![],
                    }
                }
                _ => Body::Error {
 code: 400, message: "expected request/Task".into(), taxonomy: None, details: None,
                },
            };
            let mut out = Envelope::new(my_aid, env.from.clone(), Performative::Inform, reply_body);
            out.in_reply_to = Some(env.msg_id.clone());
            out.sign(key).expect("sign");
            respond(&mut stream, "200 OK", "application/acl-envelope", &frame(&out.encode().unwrap()));
        }
        ("GET", "/events") => {
            let text = query
                .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("text=").map(str::to_owned)))
                .unwrap_or_else(|| "empty".to_string())
                .replace("%20", " ");
            let task_id = acl_core::new_msg_id();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            // progress events (§5: progress carries target_msg_id)
            for pct in [25u8, 50, 75] {
                let mut ev = Envelope::new(
                    my_aid.clone(),
                    Aid::new("http://client/"),
                    Performative::Progress,
                    Body::Progress {
                        target_msg_id: task_id.clone(),
                        percent: Some(pct),
                        note: Some(format!("processing {pct}%")),
                    },
                );
                ev.reply_with = Some(task_id.clone());
                ev.sign(key).expect("sign");
                sse_event(&mut stream, &ev);
            }
            let reversed: String = text.chars().rev().collect();
            let mut ev = Envelope::new(
                my_aid,
                Aid::new("http://client/"),
                Performative::Inform,
                Body::Result {
                    output: serde_json::json!({"reversed": reversed, "task_id": task_id}),
                    artifacts: vec![],
                },
            );
            ev.in_reply_to = Some(task_id.clone());
            ev.reply_with = Some(task_id);
            ev.sign(key).expect("sign");
            sse_event(&mut stream, &ev);
            stream.flush().unwrap();
        }
        ("GET", "/agent-card") => {
            let mut card = AgentCard {
                name: "acl-httpd".into(),
                version: "0.1".into(),
                description: "HTTP/SSE demo peer".into(),
                endpoint: my_aid.clone(),
                capabilities: vec![Capability {
                    name: "text.reverse".into(),
                    input_schema: None,
                    output_schema: None,
                    examples: None,
                }],
                auth_scheme: "ed25519".into(),
                key_id: None,
                public_key: None,
                rate_hint: None,
                valid_until: None,
                sig: None,
            };
            card.sign(key).expect("sign");
            let mut out = Envelope::new(
                my_aid,
                Aid::new("http://client/"),
                Performative::Hello,
                Body::Hello {
                    agent_card: Some(card),
                    card_hash: None,
                    nonce: vec![],
                },
            );
            out.sign(key).expect("sign");
            respond(&mut stream, "200 OK", "application/acl-envelope", &frame(&out.encode().unwrap()));
        }
        _ => {
            respond(&mut stream, "404 Not Found", "text/plain", b"not found");
        }
    }
}

fn mk_aid() -> Aid {
    Aid::new("http://127.0.0.1/agent/acl-httpd")
}
// silence unused warning if mk_aid becomes handy
#[allow(dead_code)]
fn _vk_guard() {
    let _ = VerifyingKey::from_bytes;
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 4);
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn strip_frame(body: &[u8]) -> &[u8] {
    if body.len() >= 4 {
        let n = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
        if body.len() == 4 + n {
            return &body[4..];
        }
    }
    body // also accept raw envelope
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

fn sse_event(stream: &mut TcpStream, env: &Envelope) {
    let b64 = base64_encode(&env.encode().expect("encode"));
    let ev = format!("event: acl\ndata: {b64}\n\n");
    stream.write_all(ev.as_bytes()).unwrap();
    stream.flush().unwrap();
}

/// Minimal base64 (RFC 4648, standard alphabet, padded).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}
