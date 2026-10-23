//! Interop test runner: creates a signed envelope, then reads back a
//! reply and verifies it. Used to prove an independent Python
//! implementation (written from SPEC.md only) can talk to acl-core.
//!
//! Modes:
//!   acl-interop send <out_file> <pubkey_file>   — write signed envelope, dump pubkey
//!   acl-interop recv <in_file> <pubkey_file>    — read reply, verify sig, print status

use acl_core::{read_frame, Aid, Body, Envelope, Performative, ACL_VERSION};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use std::fs;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args[1].as_str() {
        "send" => send(&args[2], &args[3]),
        "recv" => recv(&args[2], &args[3]),
        _ => eprintln!("usage: acl-interop send|recv <file> <pubkey_file>"),
    }
}

fn send(out_file: &str, pubkey_file: &str) {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let mut env = Envelope::new(
        Aid::new("did:example:rust-sender"),
        Aid::new("local:9999:py-peer"),
        Performative::Request,
        Body::Task {
            description: "reverse this string".into(),
            input: serde_json::json!({"text": "interop-test"}),
            deadline_ms: Some(30_000),
            priority: Some(3),
        },
    );
    env.reply_with = Some("interop-session-1".into());
    env.sign(&key).expect("sign");
    let payload = env.encode().expect("encode");

    // length-prefixed frame, as the spec defines for stream transports
    let mut framed = Vec::new();
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);
    fs::write(out_file, &framed).expect("write");
    fs::write(pubkey_file, key.verifying_key().to_bytes()).expect("write pubkey");
    println!(
        "SENT msg_id={} bytes={} (framed {})",
        env.msg_id,
        payload.len(),
        framed.len()
    );
    println!("msg_id={}", env.msg_id);
}

fn recv(in_file: &str, pubkey_file: &str) {
    let framed = fs::read(in_file).expect("read");
    let mut cur = &framed[..];
    let payload = read_frame(&mut cur).expect("frame").expect("frame body");
    let env = Envelope::decode(&payload).expect("decode");
    let pk_bytes: [u8; 32] = fs::read(pubkey_file)
        .expect("read pubkey")
        .try_into()
        .expect("32 bytes");
    let vk = VerifyingKey::from_bytes(&pk_bytes).expect("vk");

    let sig_ok = env.verify(&vk).expect("verify call");
    println!("PERFORMATIVE={:?}", env.performative);
    println!("VERSION={}", env.acl);
    println!("VERSION_EXPECTED={ACL_VERSION}");
    println!("SIG_OK={sig_ok}");
    println!("FROM={}", env.from.0);
    println!("IN_REPLY_TO={}", env.in_reply_to.as_deref().unwrap_or("-"));

    // build a result envelope back to the python peer
    match (&env.performative, &env.body) {
        (Performative::Request, Body::Task { input, .. }) => {
            let text = input["text"].as_str().unwrap_or_default();
            let reversed: String = text.chars().rev().collect();
            let mut reply = Envelope::new(
                Aid::new("did:example:rust-echo"),
                env.from.clone(),
                Performative::Inform,
                Body::Result {
                    output: serde_json::json!({"reversed": reversed}),
                    artifacts: vec![],
                },
            );
            reply.in_reply_to = Some(env.msg_id.clone());
            let _ = vk; // reply unsigned here; python verifies sender's sig only
            println!("ECHO={reversed}");
        }
        _ => println!("ECHO=-"),
    }
}
