//! Codec micro-benchmarks (std::time based — no deps).
//! Run: cargo test --release --test bench -- --nocapture --ignored

use acl_core::{AclError, Aid, Body, Envelope, Performative};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use std::time::Instant;

fn sample() -> Envelope {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let mut env = Envelope::new(
        Aid::local(4211, "bench"),
        Aid::new("did:example:target"),
        Performative::Request,
        Body::Task {
            description: "benchmark workload".into(),
            input: serde_json::json!({"file": "q3.pdf", "lang": "pt", "depth": 3}),
            deadline_ms: Some(60_000),
            priority: Some(5),
        },
    );
    env.sign(&key).unwrap();
    env
}

fn bench<F: FnMut()>(name: &str, mut f: F) -> f64 {
    // warmup
    for _ in 0..1000 {
        f();
    }
    const N: u32 = 50_000;
    let start = Instant::now();
    for _ in 0..N {
        f();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let ops = N as f64 / elapsed;
    println!("{name:<28} {ops:>12.0} ops/s  ({:.3} µs/op)", elapsed * 1e6 / N as f64);
    ops
}

#[test]
#[ignore] // run explicitly: cargo test --release --test bench -- --ignored --nocapture
fn codec_benchmarks() {
    let env = sample();
    let encoded = env.encode().unwrap();
    let key = SigningKey::from_bytes(&[42u8; 32]);
    let vk: VerifyingKey = key.verifying_key();

    let e = bench("encode (msgpack)", || {
        let _ = env.encode();
    });
    let d = bench("decode (msgpack)", || {
        let _ = Envelope::decode(&encoded);
    });
    let s = bench("sign (ed25519)", || {
        let mut copy = env.clone();
        copy.sig = None;
        let _ = key.sign(&copy.encode_unsigned().unwrap());
    });
    let sig64: [u8; 64] = env.sig.clone().unwrap().try_into().unwrap();
    let sig = ed25519_dalek::Signature::from_bytes(&sig64);
    let v = bench("verify (ed25519)", || {
        let _ = vk.verify(&env.encode_unsigned().unwrap(), &sig);
    });
    let _ = (e, d, s, v, key, std::marker::PhantomData::<AclError>);

    // msgpack vs json size
    let js = serde_json::to_vec(&env).unwrap();
    println!("\nenvelope size: msgpack={} json={} (−{:.0}%)",
        encoded.len(), js.len(), 100.0 * (1.0 - encoded.len() as f64 / js.len() as f64));
}
