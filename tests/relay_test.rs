//! Relay integration test: store-and-forward with did:key verification.
//!
//! 1. agent A (did:key) connects, sends an envelope for offline agent B
//! 2. agent B (did:key) connects later, receives the queued message
//! 3. signature of the queued message is verified via the from AID alone
//! 4. a tampered replay is rejected by the relay's ReplayGuard

use acl_core::did;
use acl_core::{Aid, Body, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use std::io::{BufWriter, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

fn spawn_reay(port: u16, dir: &str) -> Child {
    Command::new("target/debug/relay")
        .args(["--port", &port.to_string(), "--dir", dir])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn relay")
}

fn connect(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("relay never came up on {port}");
}

fn send_frame(s: &mut TcpStream, payload: &[u8]) {
    let mut buf = Vec::with_capacity(payload.len() + 4);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    s.write_all(&buf).expect("write");
    s.flush().expect("flush");
}

fn read_frame(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).ok()?;
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn new_agent(name: &str) -> (SigningKey, Aid) {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let aid = Aid::new(&did::verifying_key_to_did_key(&key.verifying_key()));
    let _ = name;
    (key, aid)
}

#[test]
fn relay_store_and_forward() {
    let port = 9521;
    let dir = format!("/tmp/acl-relay-test-{}", std::process::id());
    std::fs::create_dir_all(&dir).unwrap();
    let mut relay = spawn_reay(port, &dir);

    let (key_a, aid_a) = new_agent("a");
    let (key_b, aid_b) = new_agent("b");

    // --- A connects and sends a message for offline B
    let mut a = connect(port);
    let msg = Envelope::new(
        aid_a.clone(),
        aid_b.clone(),
        Performative::Request,
        Body::Task {
            description: "async work".into(),
            input: serde_json::json!({"n": 7}),
            deadline_ms: None,
            priority: None,
        },
    );
    let mut signed = msg.clone();
    signed.sign(&key_a).unwrap();
    send_frame(&mut a, &signed.encode().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(a);

    // --- B connects (sends a heartbeat from itself → triggers delivery)
    let mut b = connect(port);
    let mut heartbeat = Envelope::new(
        aid_b.clone(),
        aid_a.clone(),
        Performative::Inform,
        Body::Text { text: "b is online".into() },
    );
    heartbeat.sign(&key_b).unwrap();
    send_frame(&mut b, &heartbeat.encode().unwrap());

    // B must receive the queued message
    let got = read_frame(&mut b).expect("queued message delivered");
    let env = Envelope::decode(&got).expect("decode");
    assert_eq!(env.msg_id, signed.msg_id, "same message");
    match &env.body {
        Body::Task { description, .. } => assert_eq!(description, "async work"),
        other => panic!("unexpected body {other:?}"),
    }
    // verify with ONLY the from AID (did:key resolution, §22)
    let vk = did::did_key_to_verifying_key(&env.from.0).expect("resolve sender");
    assert!(env.verify(&vk).unwrap(), "queued message signature valid via did:key");

    // --- tampered replay: same msg_id, different content → rejected
    let mut tampered = signed.clone();
    tampered.body = Body::Task {
        description: "EVIL".into(),
        input: serde_json::json!({}),
        deadline_ms: None,
        priority: None,
    };
    // (re-sign with B's key: same msg_id, new content → ReplayGuard rejects)
    tampered.sign(&key_b).unwrap();
    send_frame(&mut b, &tampered.encode().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(300));
    // no new delivery of the tampered message: read whatever arrives, none matches EVIL
    b.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
    let mut evil_got_through = false;
    while let Some(bytes) = read_frame(&mut b) {
        if let Ok(e) = Envelope::decode(&bytes) {
            if matches!(&e.body, Body::Task { description, .. } if description == "EVIL") {
                evil_got_through = true;
            }
        }
    }
    assert!(!evil_got_through, "tampered/replayed message must not be delivered");

    // store is now empty for B (message was consumed)
    let pending = {
        let dir_b = format!("{dir}/{}", {
            use sha1::{Digest, Sha1};
            let mut h = Sha1::new();
            h.update(aid_b.0.as_bytes());
            h.finalize().iter().map(|x| format!("{x:02x}")).collect::<String>()
        });
        std::fs::read_dir(dir_b).map(|d| d.count()).unwrap_or(0)
    };
    assert_eq!(pending, 0, "mailbox drained after delivery");

    let _ = relay.kill();
    let _ = std::fs::remove_dir_all(dir);
    let _ = BufWriter::new(std::io::stdout().lock());
}
