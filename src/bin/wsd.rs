//! acl-wsd — ACL over WebSocket (SPEC.md §9.3).
//!
//! Minimal RFC 6455 server: HTTP upgrade handshake, then binary frames,
//! each frame = framed ACL envelope. Serves the same semantics as
//! acl-httpd's POST /: request/Task → signed inform/Result.

use acl_core::{Aid, Body, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use sha1::{Digest, Sha1};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// §37: token from query param (`ws://...?token=...`)
fn authorized(path_q: Option<&str>, token: Option<&str>) -> bool {
    match token {
        None => true,
        Some(t) => path_q
            .and_then(|q| {
                q.split('&').find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    (k == "token").then(|| v.to_string())
                })
            })
            .map(|v| v == t)
            .unwrap_or(false),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(9300);
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let my_aid = Aid::new("ws://127.0.0.1/agent/acl-wsd");
    std::fs::write("/tmp/acl-wsd.pub", key.verifying_key().to_bytes()).expect("pub");
    eprintln!("[acl-wsd] listening on 127.0.0.1:{port}");

    let token: Option<String> = args
        .iter()
        .position(|a| a == "--token")
        .and_then(|i| args.get(i + 1))
        .cloned();
    match &token {
        Some(_) => eprintln!("[acl-wsd] token auth ENABLED"),
        None => eprintln!("[acl-wsd] token auth DISABLED (dev mode)"),
    }
    for stream in TcpListener::bind(("127.0.0.1", port)).expect("bind").incoming() {
        if let Ok(s) = stream {
            let _ = serve(s, &key, &my_aid, token.as_deref());
        }
    }
}

fn serve(
    mut stream: std::net::TcpStream,
    key: &SigningKey,
    my_aid: &Aid,
    token: Option<&str>,
) -> std::io::Result<()> {
    // --- read HTTP upgrade request
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut ws_key = String::new();
    let mut upgrade_path = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line == "\r\n" {
            break;
        }
        if line.starts_with("GET ") {
            upgrade_path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
        }
        if let Some(v) = line.strip_prefix("Sec-WebSocket-Key:") {
            ws_key = v.trim().to_string();
        }
    }
    if ws_key.is_empty() {
        return Ok(());
    }
    // §37: token via query param on the upgrade path
    let path_q = upgrade_path.split_once('?').map(|(_, q)| q.to_string());
    if !authorized(path_q.as_deref(), token) {
        let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        return Ok(());
    }
    // accept = base64(sha1(key + GUID))
    let mut h = Sha1::new();
    h.update(ws_key.as_bytes());
    h.update(WS_GUID);
    let accept = b64(&h.finalize());
    stream.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;
    eprintln!("[acl-wsd] websocket established");

    // --- message loop: read ws frames, answer with signed envelopes
    let mut buf = [0u8; 65536];
    loop {
        read_exact_n(&mut stream, &mut buf, 2)?;
        let _fin_op = buf[0];
        let masked = buf[1] & 0x80 != 0;
        let mut len = (buf[1] & 0x7f) as usize;
        if len == 126 {
            read_exact_n(&mut stream, &mut buf, 2)?;
            len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        } else if len == 127 {
            read_exact_n(&mut stream, &mut buf, 8)?;
            len = u64::from_be_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]])
                as usize;
        }
        if !masked {
            eprintln!("[acl-wsd] client frame must be masked");
            return Ok(());
        }
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask)?;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload)?;
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        // close frame?
        if _fin_op & 0x0f == 0x8 {
            eprintln!("[acl-wsd] close frame, ending");
            return Ok(());
        }
        // payload = framed ACL envelope
        let env = match Envelope::decode(&payload) {
            Ok(e) => e,
            Err(_) => {
                let n = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
                match Envelope::decode(&payload[4..4 + n]) {
                    Ok(e) => e,
                    Err(_) => return Ok(()),
                }
            }
        };
        eprintln!("[acl-wsd] <- {:?} from={}", env.performative, env.from.0);

        let reply_body = match (&env.performative, &env.body) {
            (Performative::Request, Body::Task { input, .. }) => {
                let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Body::Result {
                    output: serde_json::json!({"reversed": text.chars().rev().collect::<String>(), "via": "ws"}),
                    artifacts: vec![],
                }
            }
            _ => Body::Error {
 code: 400, message: "expected request/Task".into(), taxonomy: None, details: None },
        };
        let mut out = Envelope::new(my_aid.clone(), env.from.clone(), Performative::Inform, reply_body);
        out.in_reply_to = Some(env.msg_id.clone());
        out.sign(key).expect("sign");
        let encoded = out.encode().expect("encode");
        send_ws_frame(&mut stream, &encoded)?;
        eprintln!("[acl-wsd] -> Inform");
    }
}

fn read_exact_n(stream: &mut std::net::TcpStream, buf: &mut [u8], n: usize) -> std::io::Result<()> {
    stream.read_exact(&mut buf[..n])
}

fn send_ws_frame(stream: &mut std::net::TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x82u8]; // FIN + binary opcode
    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else if len < 65536 {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
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
