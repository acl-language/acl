//! acl-discover — generates the agent-discovery artifacts (spec §7 + §22):
//!
//!   agent-card.json — self-signed AgentCard describing THIS reference
//!                     implementation's capabilities (§7: card carries its
//!                     own public_key + sig, verifiable without trust)
//!   did.json        — did:web DID document for the deployed endpoint
//!
//! The signing key is EPHEMERAL per build (identity = capability discovery,
//! not authentication). Usage: acl-discover <out-dir> [endpoint-url]

use acl_core::did;
use acl_core::{AgentCard, Capability};
use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_dir = args.get(1).cloned().unwrap_or_else(|| ".".into());
    let endpoint = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "https://acl-language.github.io/acl/".into());

    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let did_key = did::verifying_key_to_did_key(&key.verifying_key());

    // ---- AgentCard (§7): self-signed capability descriptor
    let mut card = AgentCard {
        name: "acl-reference".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "ACL reference implementation — speaks the full protocol: \
            14 performatives, content language (typed facts/actions/queries/\
            commitments with provenance), dialogue rules, runtime schema \
            validation, did:key identity, store-and-forward relay."
            .into(),
        endpoint: acl_core::Aid::new(&endpoint),
        capabilities: vec![
            cap("acl.speak", Some(json!({"type": "object", "description": "any §5 performative with §6 body"}))),
            cap("acl.cl.query", Some(json!({"type": "object", "required": ["predicate"], "properties": {"predicate": {"type": "string"}, "args": {"type": "object"}}}))),
            cap("acl.cl.action", None),
            cap("acl.cl.commitment", None),
            cap("acl.schema.validate", None),
            cap("acl.relay.store_forward", None),
            cap("acl.did.resolve", Some(json!({"type": "object", "description": "did:key offline + did:web URL construction"}))),
            cap("acl.cnp.negotiate", None),
        ],
        auth_scheme: "ed25519".into(),
        key_id: Some(did_key.clone()),
        public_key: Some(key.verifying_key().to_bytes().to_vec()),
        rate_hint: None,
        valid_until: None,
        sig: None,
    };
    card.sign(&key).expect("card sign");

    let card_json = serde_json::to_string_pretty(&card).expect("card json");
    std::fs::create_dir_all(format!("{out_dir}/.well-known")).expect("mkdir");
    std::fs::write(format!("{out_dir}/.well-known/agent-card.json"), &card_json).expect("write card");
    println!("agent-card: {} (self-signed, pubkey embedded — verify per §7)", did_key);

    // ---- DID document (§22 did:web)
    let did_doc = json!({
        "@context": ["https://www.w3.org/ns/did/v1",
                     "https://w3id.org/security/suites/ed25519-2020/v1"],
        "id": "did:web:acl-language.github.io:acl",
        "verificationMethod": [{
            "id": format!("did:web:acl-language.github.io:acl#{}", did_key),
            "type": "Ed25519VerificationKey2020",
            "controller": "did:web:acl-language.github.io:acl",
            "publicKeyMultibase": multibase(&key.verifying_key().to_bytes()),
        }],
        "authentication": [format!("did:web:acl-language.github.io:acl#{}", did_key)],
        "service": [{
            "id": "did:web:acl-language.github.io:acl#agent-card",
            "type": "AgentCard",
            "serviceEndpoint": format!("{endpoint}.well-known/agent-card.json"),
        }],
    });
    std::fs::write(
        format!("{out_dir}/.well-known/did.json"),
        serde_json::to_string_pretty(&did_doc).expect("did json"),
    )
    .expect("write did");
    println!("did.json: did:web:acl-language.github.io:acl");
}

fn cap(name: &str, schema: Option<serde_json::Value>) -> Capability {
    Capability { name: name.into(), input_schema: schema, output_schema: None, examples: None }
}

/// multibase base58btc ('z' prefix) of the raw 32-byte pubkey
fn multibase(pk: &[u8; 32]) -> String {
    format!("z{}", bs58::encode(pk).into_string())
}
