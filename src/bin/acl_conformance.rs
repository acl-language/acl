//! acl-conformance — the certification suite (spec §23).
//!
//! Runs the named checks an implementation must pass to be "ACL
//! compliant", prints a report, exits 0 only if all pass. External
//! implementations can diff their results against this binary.

use acl_core::cl::{self, ClAction, ClExpr, CommitmentStatus};
use acl_core::dialogue::dialogue_legal;
use acl_core::idempotency::IdempotencyCache;
use acl_core::idl;
use acl_core::schema::RuntimeSchema;
use acl_core::{batch, did, sealed, Aid, Body, Envelope, Performative};
use ed25519_dalek::SigningKey;
use rand::Rng;
use std::collections::BTreeMap;

struct Check {
    name: &'static str,
    level: u8,
}

const CHECKS: &[Check] = &[
    Check { name: "C1 codec roundtrip", level: 1 },
    Check { name: "C2 signature + tamper", level: 1 },
    Check { name: "C3 stream framing", level: 1 },
    Check { name: "C4 CL grammar matrix", level: 2 },
    Check { name: "C5 dialogue rules", level: 2 },
    Check { name: "C6 idempotency", level: 2 },
    Check { name: "C7 sealed envelopes", level: 3 },
    Check { name: "C8 schema validation", level: 3 },
    Check { name: "C9 DID resolution", level: 3 },
    Check { name: "C10 batch verification", level: 3 },
];

fn key() -> SigningKey {
    let mut s = [0u8; 32];
    rand::rng().fill_bytes(&mut s);
    SigningKey::from_bytes(&s)
}

fn c1() -> bool {
    let bodies = vec![
        (Performative::Request, Body::Task { description: "t".into(), input: serde_json::json!({"x":1}), deadline_ms: None, priority: None }),
        (Performative::Inform, Body::Text { text: "hi".into() }),
        (Performative::Query, Body::Query { subject: "s".into(), parameters: None }),
        (Performative::Failure, Body::Failure { target_msg_id: "m".into(), code: 500, reason: "r".into(), recoverable: false }),
    ];
    bodies.iter().all(|(p, b)| {
        let e = Envelope::new(Aid::local(1, "c"), Aid::new("did:x:y"), *p, b.clone());
        Envelope::decode(&e.encode().unwrap()).map(|r| r.msg_id == e.msg_id).unwrap_or(false)
    })
}

fn c2() -> bool {
    let k = key();
    let mut env = Envelope::new(
        Aid::local(1, "c"),
        Aid::new("did:x:y"),
        Performative::Inform,
        Body::Text { text: "x".into() },
    );
    env.sign(&k).unwrap();
    let ok = env.verify(&k.verifying_key()).unwrap();
    env.ttl = 1;
    ok && !env.verify(&k.verifying_key()).unwrap()
}

fn c3() -> bool {
    let env = Envelope::new(
        Aid::local(1, "c"),
        Aid::new("did:x:y"),
        Performative::Inform,
        Body::Text { text: "frame".into() },
    );
    let payload = env.encode().unwrap();
    let mut stream = Vec::new();
    acl_core::write_frame(&mut stream, &payload);
    let mut cur = &stream[..];
    matches!(acl_core::read_frame(&mut cur), Ok(Some(p)) if p == payload)
}

fn c4() -> bool {
    let good = cl::well_formed(
        Performative::Inform,
        &[ClExpr::fact("f", serde_json::json!({}))],
    )
    .is_ok();
    let bad = cl::well_formed(
        Performative::Inform,
        &[ClExpr::action("a", serde_json::json!({}))],
    )
    .is_err();
    good && bad
}

fn c5() -> bool {
    dialogue_legal(Performative::Request, Performative::Inform)
        && dialogue_legal(Performative::Query, Performative::Inform)
        && !dialogue_legal(Performative::Propose, Performative::Inform)
        && !dialogue_legal(Performative::Bye, Performative::Inform)
}

fn c6() -> bool {
    let mut cache = IdempotencyCache::new();
    let mut e = Envelope::new(
        Aid::local(1, "c"),
        Aid::new("did:x:y"),
        Performative::Request,
        Body::Text { text: "dup".into() },
    );
    e.msg_id = "fixed-id".into();
    cache.check(&e, 1000) && !cache.check(&e, 2000)
}

fn c7() -> bool {
    use x25519_dalek::{PublicKey, StaticSecret};
    let sender = StaticSecret::from({
        let mut s = [0u8; 32];
        rand::rng().fill_bytes(&mut s);
        s
    });
    let recipient = StaticSecret::from({
        let mut s = [0u8; 32];
        rand::rng().fill_bytes(&mut s);
        s
    });
    let inner = Envelope::new(
        Aid::local(1, "c"),
        Aid::new("did:x:y"),
        Performative::Inform,
        Body::Text { text: "secret".into() },
    );
    let sealed = sealed::seal(&inner, &sender, &PublicKey::from(&recipient)).unwrap();
    let opened = sealed::open(&sealed, &recipient).unwrap();
    match opened.body {
        Body::Text { text } => text == "secret",
        _ => false,
    }
}

fn c8() -> bool {
    let schema_json = br#"{"namespace":"t.v","version":1,"aliases":{},
        "predicates":{"f":{"params":{"x":"string"},"ret":"bool"}},
        "actions":{}}"#;
    let schema = RuntimeSchema::from_json(schema_json).unwrap();
    schema
        .validate(&ClExpr::fact("t.v.f", serde_json::json!({"x": "ok"})))
        .is_ok()
        && schema
            .validate(&ClExpr::fact("t.v.f", serde_json::json!({"x": 1})))
            .is_err()
}

fn c9() -> bool {
    let k = key();
    let did = did::verifying_key_to_did_key(&k.verifying_key());
    let resolved = did::did_key_to_verifying_key(&did).unwrap();
    resolved.to_bytes() == k.verifying_key().to_bytes()
}

fn c10() -> bool {
    let k = key();
    let mut env = Envelope::new(
        Aid::local(1, "c"),
        Aid::new("did:x:y"),
        Performative::Inform,
        Body::Text { text: "batch".into() },
    );
    env.sig = {
        use ed25519_dalek::Signer;
        Some(k.sign(&env.encode_unsigned().unwrap()).to_bytes().to_vec())
    };
    let vk = k.verifying_key();
    batch::verify_batch(&[(&env, &vk)]).is_ok()
}

fn main() {
    let results: Vec<(&Check, bool)> = CHECKS
        .iter()
        .map(|c| {
            let ok = match c.name {
                n if n.starts_with("C1") => c1(),
                n if n.starts_with("C2") => c2(),
                n if n.starts_with("C3") => c3(),
                n if n.starts_with("C4") => c4(),
                n if n.starts_with("C5") => c5(),
                n if n.starts_with("C6") => c6(),
                n if n.starts_with("C7") => c7(),
                n if n.starts_with("C8") => c8(),
                n if n.starts_with("C9") => c9(),
                n if n.starts_with("C10") => c10(),
                _ => false,
            };
            (c, ok)
        })
        .collect();

    println!("ACL Conformance Report");
    println!("======================");
    let mut all_ok = true;
    let mut max_level = 0u8;
    for (c, ok) in &results {
        println!("[{}] L{}  {}", if *ok { "PASS" } else { "FAIL" }, c.level, c.name);
        if *ok && c.level > max_level {
            max_level = c.level;
        }
        if !*ok {
            all_ok = false;
        }
    }
    println!("----------------------");
    if all_ok {
        println!("COMPLIANT: ACL level {max_level} (all {CHECKS_len} checks passed)", CHECKS_len = CHECKS.len());
    } else {
        println!("NON-COMPLIANT");
    }
    std::process::exit(if all_ok { 0 } else { 1 });
}
