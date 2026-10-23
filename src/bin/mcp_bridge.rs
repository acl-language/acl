//! acl-mcp-bridge — speaks ACL on one side, real MCP (JSON-RPC/stdio) on the other.
//!
//! Spawns an MCP server (node server.js), performs the MCP initialize
//! handshake, then serves ACL clients on its own stdio:
//!   request/Task          → tools/call  → inform/Result
//!   query/agent.card      → tools/list  → Hello + signed AgentCard
//!
//! Usage: acl-mcp-bridge --mcp <cmd> [args...]

use acl_core::{AgentCard, Aid, Body, Capability, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--mcp").expect("--mcp <cmd>");
    let mut cmd_end = args.len();
    let mut pub_out = String::new();
    if let Some(pi) = args.iter().position(|a| a == "--pub-out") {
        pub_out = args[pi + 1].clone();
        cmd_end = cmd_end.min(pi);
    }
    let cmd_parts: Vec<String> = args[idx + 1..cmd_end].to_vec();
    eprintln!("[bridge] starting MCP server: {cmd_parts:?}");

    let mut child = Command::new(&cmd_parts[0])
        .args(&cmd_parts[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    let mut mcp_in = child.stdin.take().expect("stdin");
    let mut mcp_out = BufReader::new(child.stdout.take().expect("stdout"));

    // --- MCP initialize handshake
    rpc(&mut mcp_in, &mut mcp_out, &mut Some(&mut child), "initialize", serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "acl-mcp-bridge", "version": "0.1.0"}
    }));
    notify(&mut mcp_in, "notifications/initialized", serde_json::json!({}));
    eprintln!("[bridge] MCP handshake ok");

    // --- signing key for the bridge's own ACL identity
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    if !pub_out.is_empty() {
        std::fs::write(&pub_out, key.verifying_key().to_bytes()).expect("write pub");
    }
    let my_aid = Aid::local(std::process::id(), "mcp-bridge");

    // --- serve ACL clients on our stdin
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = std::io::stdout();
    while let Ok(Some(payload)) = acl_core::read_frame(&mut reader) {
        let env = match Envelope::decode(&payload) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[bridge] decode error: {e}");
                continue;
            }
        };
        eprintln!("[bridge] <- {:?}", env.performative);

        let reply_body = match (&env.performative, &env.body) {
            (Performative::Request, Body::Task { input, .. }) => {
                let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let result = rpc(
                    &mut mcp_in,
                    &mut mcp_out,
                    &mut Some(&mut child),
                    "tools/call",
                    serde_json::json!({
                        "name": "text.reverse",
                        "arguments": {"text": text}
                    }),
                );
                let reversed = result["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                Body::Result {
                    output: serde_json::json!({"reversed": reversed, "via": "mcp"}),
                    artifacts: vec![],
                }
            }
            (Performative::Query, Body::Query { subject, .. }) if subject == "agent.card" => {
                // MCP tools/list → ACL AgentCard
                let result = rpc(
                    &mut mcp_in,
                    &mut mcp_out,
                    &mut Some(&mut child),
                    "tools/list",
                    serde_json::json!({}),
                );
                let tools = result["tools"].as_array().cloned().unwrap_or_default();
                let mut card = AgentCard {
                    name: "mcp-bridged".into(),
                    version: "0.1".into(),
                    description: format!("MCP tools exposed as ACL capabilities ({} tools)", tools.len()),
                    endpoint: my_aid.clone(),
                    capabilities: tools
                        .iter()
                        .map(|t| Capability {
                            name: t["name"].as_str().unwrap_or_default().to_string(),
                            input_schema: Some(t["inputSchema"].clone()),
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
                };
                card.sign(&key).expect("card sign");
                Body::Hello {
                    agent_card: Some(card),
                    card_hash: None,
                    nonce: vec![],
                }
            }
            _ => Body::Error {
 code: 400, message: "bridge supports request/Task and query/agent.card".into(), taxonomy: None, details: None,
            },
        };

        let perf = match reply_body {
            Body::Hello { .. } => Performative::Hello,
            Body::Error { .. } => Performative::Error,
            _ => Performative::Inform,
        };
        let mut out = Envelope::new(my_aid.clone(), env.from.clone(), perf, reply_body);
        out.in_reply_to = Some(env.msg_id.clone());
        out.sign(&key).expect("sign");
        let encoded = out.encode().expect("encode");
        let mut framed = Vec::with_capacity(encoded.len() + 4);
        framed.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        framed.extend_from_slice(&encoded);
        stdout.write_all(&framed).expect("write");
        stdout.flush().expect("flush");
        eprintln!("[bridge] -> {:?}", out.performative);
    }
    let _ = child.kill();
}

fn rpc(
    w: &mut dyn Write,
    r: &mut dyn BufRead,
    _child: &mut Option<&mut Child>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let req = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    writeln!(w, "{req}").expect("write rpc");
    w.flush().expect("flush");
    let mut line = String::new();
    r.read_line(&mut line).expect("read rpc");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("parse rpc");
    if let Some(err) = resp.get("error") {
        eprintln!("[bridge] MCP error on {method}: {err}");
    }
    resp["result"].clone()
}

fn notify(w: &mut dyn Write, method: &str, params: serde_json::Value) {
    let n = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
    writeln!(w, "{n}").expect("write notify");
    w.flush().expect("flush");
}
