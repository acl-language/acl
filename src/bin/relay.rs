//! acl-relay — persistent store-and-forward message relay (spec §24).
//!
//! Turns point-to-point ACL into an asynchronous system: agents connect
//! over TCP, drop envelopes addressed to anyone, and pick up their own
//! mail when online. Messages for offline agents persist on disk until
//! delivered or expired (TTL).
//!
//! Wire protocol (all framed per §3):
//!   1. client connects, sends any envelope; its `from` AID is its mailbox
//!   2. relay verifies (did:key senders are verified with ONLY the from
//!      field, §22), replays (§24) and rate-limits (§24)
//!   3. envelope is enqueued for `to`
//!   4. relay delivers everything pending for the client's `from`,
//!      BATCH-VERIFIED (§23 batch) before sending
//!   5. expired envelopes (envelope.ttl) are dropped on sweep
//!
//! Usage: acl-relay --port N --dir <store-dir>

use acl_core::batch;
use acl_core::did;
use acl_core::replay::{RateLimiter, ReplayGuard};
use acl_core::{Aid, Envelope};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::{BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(9500);
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "/tmp/acl-relay-store".into());

    std::fs::create_dir_all(&dir).expect("store dir");
    let store = Store { dir: PathBuf::from(&dir) };
    eprintln!("[acl-relay] listening on 127.0.0.1:{port}, store={dir}");

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    for stream in listener.incoming().flatten() {
        let store = store.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &store);
        });
    }
}

// ---------------------------------------------------------------- store

#[derive(Clone)]
struct Store {
    dir: PathBuf,
}

impl Store {
    fn mbox(&self, aid: &str) -> PathBuf {
        let mut h = Sha1::new();
        h.update(aid.as_bytes());
        self.dir.join(hex_digest(&h))
    }

    fn enqueue(&self, env: &Envelope) -> std::io::Result<()> {
        let dir = self.mbox(&env.to.0);
        std::fs::create_dir_all(&dir)?;
        let bytes = env.encode().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(dir.join(format!("{}.bin", env.msg_id)), bytes)
    }

    fn pending(&self, aid: &str) -> Vec<Envelope> {
        let dir = self.mbox(aid);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            files.sort(); // FIFO by msg_id (ULIDs would be better; hex ids sort fine within a run)
            for f in files {
                if let Ok(bytes) = std::fs::read(&f) {
                    if let Ok(env) = Envelope::decode(&bytes) {
                        out.push(env);
                    }
                }
            }
        }
        out
    }

    fn remove(&self, aid: &str, msg_id: &str) {
        let _ = std::fs::remove_file(self.mbox(aid).join(format!("{msg_id}.bin")));
    }

    fn sweep_expired(&self, aid: &str, now_ms: i64) -> usize {
        let mut dropped = 0;
        for env in self.pending(aid) {
            if env.is_expired(now_ms) {
                self.remove(aid, &env.msg_id);
                dropped += 1;
            }
        }
        dropped
    }
}

fn hex_digest(h: &Sha1) -> String {
    h.clone().finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------- handling

fn handle(mut stream: TcpStream, store: &Store) -> std::io::Result<()> {
    let mut replay = ReplayGuard::new(300_000); // 5-min replay window
    let mut limiter = RateLimiter::new(100, 0.5); // 100 burst, 1 msg / 2ms
    let client: Option<Aid> = None;
    let mut client = client;
    let mut inbox = BufWriter::new(stream.try_clone()?);

    loop {
        let payload = match read_frame(&mut stream) {
            Ok(Some(p)) => p,
            Ok(None) | Err(_) => return Ok(()), // clean close or error
        };
        let env = match Envelope::decode(&payload) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[acl-relay] decode error: {e}");
                continue;
            }
        };

        let now = now_ms();
        if !limiter.allow(now) {
            eprintln!("[acl-relay] rate limited {:?}", env.from.0);
            continue;
        }
        if !replay.check(&env, now) {
            eprintln!("[acl-relay] replay rejected: {} from {:?}", env.msg_id, env.from.0);
            continue;
        }

        // verify: did:key senders verify with ONLY the from field (§22)
        let sig_ok = match did::did_key_to_verifying_key(&env.from.0) {
            Ok(vk) => env.verify(&vk).unwrap_or(false),
            Err(_) => true, // unresolvable scheme (e.g. local:) — no sig check
        };
        if !sig_ok {
            eprintln!("[acl-relay] INVALID signature: {} from {:?}", env.msg_id, env.from.0);
            continue;
        }

        // first message identifies the client's mailbox
        if client.is_none() {
            client = Some(env.from.clone());
            let who = client.as_ref().unwrap();
            eprintln!("[acl-relay] client identified: {}", who.0);
            // deliver everything pending for this client
            deliver_pending(&mut inbox, store, &who.0)?;
        }

        // expired? drop on arrival
        if env.is_expired(now) {
            eprintln!("[acl-relay] expired on arrival: {}", env.msg_id);
            continue;
        }

        // enqueue for the recipient
        store.enqueue(&env)?;
        eprintln!(
            "[acl-relay] queued {} for {} (from {:?})",
            env.msg_id, env.to.0, env.from.0
        );
    }
}

/// Deliver all pending messages for `aid`, BATCH-VERIFIED first (§23 C10).
/// Senders with resolvable did:key AIDs get real verification; messages
/// whose signature fails are dropped before delivery.
fn deliver_pending(out: &mut BufWriter<TcpStream>, store: &Store, aid: &str) -> std::io::Result<()> {
    store.sweep_expired(aid, now_ms());
    let pending = store.pending(aid);
    if pending.is_empty() {
        return Ok(());
    }

    // resolve each sender's key via did:key (batch verify needs keys)
    let mut pairs: Vec<(&Envelope, VerifyingKey)> = Vec::new();
    let mut unverifiable = Vec::new();
    for env in &pending {
        match did::did_key_to_verifying_key(&env.from.0) {
            Ok(vk) => pairs.push((env, vk)),
            Err(_) => unverifiable.push(env),
        }
    }

    // map back to owned keys for the batch call
    let keylist: Vec<VerifyingKey> = pairs.iter().map(|(_, vk)| *vk).collect();
    let pair_refs: Vec<(&Envelope, &VerifyingKey)> =
        pairs.iter().zip(&keylist).map(|((e, _), k)| (*e, k)).collect();
    let bad_ids: std::collections::HashSet<String> = match batch::verify_batch(&pair_refs) {
        Ok(()) => std::collections::HashSet::new(),
        Err(bad) => bad.iter().filter_map(|i| pairs.get(*i).map(|(e, _)| e.msg_id.clone())).collect(),
    };

    let mut delivered = 0;
    for env in &pending {
        if bad_ids.contains(&env.msg_id) {
            eprintln!("[acl-relay] batch-verify FAILED: {} — dropped", env.msg_id);
            store.remove(aid, &env.msg_id);
            continue;
        }
        if unverifiable.iter().any(|u| u.msg_id == env.msg_id) {
            eprintln!("[acl-relay] unverifiable sender: {} — delivering anyway", env.msg_id);
        }
        let encoded = env.encode().map_err(std::io::Error::other)?;
        out.write_all(&(encoded.len() as u32).to_be_bytes())?;
        out.write_all(&encoded)?;
        store.remove(aid, &env.msg_id);
        delivered += 1;
    }
    out.flush()?;
    eprintln!("[acl-relay] delivered {delivered} message(s) to {aid}");
    Ok(())
}

fn read_frame(mut s: &TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match s.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// keep imports honest (SigningKey unused directly; key comes from did:key)
#[allow(dead_code)]
fn _unused(k: &SigningKey) {
    let _ = k.sign(&[0u8]);
}
