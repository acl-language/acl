//! RDF / JSON-LD mapping (spec §26): export CL facts as JSON-LD claims
//! so ACL vocabularies interoperate with the RDF/Linked-Data ecosystem
//! (schema.org, W3C ontologies). Scope: mapping layer, not a full RDF lib.

use crate::cl::{ClAction, ClExpr, CommitmentStatus};
use serde_json::{json, Value};

/// Base IRI for ACL-mapped vocabularies: `https://acl-language.dev/ns/<ns>/`.
fn term_iri(base: &str, predicate: &str) -> String {
    let iri = if predicate.contains('.') {
        // dotted vocabulary name: acl.std.fs.file_exists → base + fs/file_exists
        let parts: Vec<&str> = predicate.split('.').collect();
        let (vocab, local) = parts.split_at(parts.len() - 1);
        format!("{}{}/{}", base, vocab.join("."), local.join("."))
    } else {
        format!("{}local/{}", base, predicate)
    };
    iri
}

/// Convert one CL fact to a JSON-LD claim node.
///
/// The claim records: the predicate IRI, the arguments as properties,
/// truth as `acl:holds`, and (when present) the epistemic metadata (§6.4).
pub fn fact_to_jsonld(
    expr: &ClExpr,
    base_iri: &str,
    msg_id: &str,
    assertor: &str,
) -> Result<Value, String> {
    let (predicate, args, truth, modality, provenance, confidence, valid_until_ms) = match expr {
        ClExpr::Fact { predicate, args, truth, modality, provenance, confidence, valid_until_ms } => {
            (predicate, args, truth, modality, provenance, confidence, valid_until_ms)
        }
        other => return Err(format!("fact_to_jsonld expects a Fact, got {other:?}")),
    };

    let mut node = json!({
        "@id": format!("urn:acl:claim:{msg_id}"),
        "@type": "acl:Claim",
        "acl:assertedBy": { "@id": assertor },
        "acl:holds": truth,
        "acl:about": { "@id": term_iri(base_iri, predicate) },
    });

    // args become typed properties: acl:arg_<name>
    if let Some(obj) = node.as_object_mut() {
        for (k, v) in args {
            obj.insert(format!("acl:arg_{}", k), v.clone());
        }
        if let Some(m) = modality {
            obj.insert("acl:modality".into(), json!(format!("{m:?}").to_lowercase()));
        }
        if let Some(p) = provenance {
            obj.insert("acl:provenance".into(), json!(p));
        }
        if let Some(c) = confidence {
            obj.insert("acl:confidence".into(), json!(c));
        }
        if let Some(v) = valid_until_ms {
            obj.insert("acl:validUntil".into(), json!(v));
        }
    }
    Ok(node)
}

/// Build a compact JSON-LD `@context` for a vocabulary namespace.
pub fn context_for(base_iri: &str, namespace: &str) -> Value {
    json!({
        "@context": {
            "acl": "https://acl-language.dev/ns/core/",
            "vocab": format!("{}{}/", base_iri, namespace),
            "Claim": "acl:Claim",
            "holds": "acl:holds",
            "assertedBy": { "@id": "acl:assertedBy", "@type": "@id" },
            "about": { "@id": "acl:about", "@type": "@id" },
        }
    })
}

/// Export a full Cl body (all facts) as a JSON-LD graph.
pub fn cl_to_graph(
    exprs: &[ClExpr],
    base_iri: &str,
    msg_id: &str,
    assertor: &str,
) -> Result<Value, String> {
    let mut graph = Vec::new();
    for e in exprs {
        match e {
            ClExpr::Fact { .. } => graph.push(fact_to_jsonld(e, base_iri, msg_id, assertor)?),
            ClExpr::Commitment { debtor, creditor, act, deadline_ms, status } => {
                graph.push(json!({
                    "@id": format!("urn:acl:commitment:{msg_id}"),
                    "@type": "acl:Commitment",
                    "acl:debtor": { "@id": debtor },
                    "acl:creditor": { "@id": creditor },
                    "acl:act": { "@id": term_iri(base_iri, &act.name) },
                    "acl:deadline": deadline_ms,
                    "acl:status": format!("{status:?}").to_lowercase(),
                }));
            }
            // queries/actions are speech acts, not claims: skipped in the graph
            _ => {}
        }
    }
    let mut doc = context_for(base_iri, "core");
    doc["@graph"] = json!(graph);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fact() -> ClExpr {
        ClExpr::Fact {
            predicate: "acl.std.fs.file_exists".into(),
            args: BTreeMap::from([("path".into(), json!("/tmp/x"))]),
            truth: true,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }
    }

    #[test]
    fn fact_maps_to_jsonld_claim() {
        let node = fact_to_jsonld(&fact(), "https://acl-language.dev/ns/", "m1", "did:key:zabc").unwrap();
        assert_eq!(node["@type"], "acl:Claim");
        assert_eq!(node["acl:holds"], true);
        assert_eq!(node["acl:arg_path"], "/tmp/x");
        assert_eq!(node["acl:assertedBy"]["@id"], "did:key:zabc");
        // dotted predicate → vocab IRI
        assert_eq!(node["acl:about"]["@id"], "https://acl-language.dev/ns/acl.std.fs/file_exists");
    }

    #[test]
    fn non_fact_rejected() {
        assert!(fact_to_jsonld(&ClExpr::action("t", json!({})), "b", "m", "a").is_err());
    }

    #[test]
    fn graph_includes_commitments() {
        let c = ClExpr::commitment(
            "did:key:z1",
            "did:key:z2",
            ClAction { name: "acl.std.fs.file_read".into(), params: json!({}), result_schema: None },
            Some(1700000000000),
            CommitmentStatus::Active,
        );
        let doc = cl_to_graph(&[fact(), c], "https://acl-language.dev/ns/", "m1", "did:key:z1").unwrap();
        let graph = doc["@graph"].as_array().unwrap();
        assert_eq!(graph.len(), 2);
        assert_eq!(graph[1]["@type"], "acl:Commitment");
        assert_eq!(graph[1]["acl:status"], "active");
        assert!(doc["@context"]["vocab"].is_string());
    }

    #[test]
    fn queries_and_actions_skipped() {
        let doc = cl_to_graph(
            &[ClExpr::query("f", json!({})), ClExpr::action("a", json!({}))],
            "b", "m", "a",
        )
        .unwrap();
        assert!(doc["@graph"].as_array().unwrap().is_empty());
    }
}
