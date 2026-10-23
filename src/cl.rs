//! Content Language (CL) — spec §6.2.
//!
//! The shared grammar agents use *inside* envelope bodies: facts about the
//! world, typed actions, queries, and commitments. This is what makes ACL a
//! communication language (agent-agnostic semantics) rather than only a
//! transport protocol.
//!
//! Design notes:
//! - `Fact.args` is a BTreeMap so serialization is canonical (identical
//!   bytes regardless of insertion order) — required for stable signatures.
//! - `well_formed` is the normative grammar matrix (§6.2): which CL ops a
//!   performative may carry. Receivers SHOULD validate; encoders MUST emit
//!   well-formed bodies.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Status of a commitment (§6.2 lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitmentStatus {
    Proposed,
    Active,
    Fulfilled,
    Breached,
    Cancelled,
}

impl CommitmentStatus {
    /// Legal transitions of the commitment lifecycle.
    pub fn can_transition(&self, to: CommitmentStatus) -> bool {
        use CommitmentStatus::*;
        matches!(
            (self, to),
            (Proposed, Active)
                | (Proposed, Cancelled)
                | (Active, Fulfilled)
                | (Active, Breached)
                | (Active, Cancelled)
        )
    }
}

/// Epistemic modality of a fact (spec §6.4): how the sender holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Epistemic {
    /// Sender asserts this as its own observation/authority.
    Asserted,
    /// Sender believes it (second-hand, not verified).
    Believed,
    /// Sender verified it directly.
    Known,
}

/// An action expression: `name(params) -> result_schema?`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClAction {
    pub name: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<serde_json::Value>,
}

/// One content-language expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ClExpr {
    /// Assert that `predicate(args)` holds (or not, `truth: false`).
    #[serde(rename = "fact")]
    Fact {
        predicate: String,
        #[serde(default)]
        args: BTreeMap<String, serde_json::Value>,
        #[serde(default = "default_true")]
        truth: bool,
        /// §6.4 epistemic modality (default: Asserted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modality: Option<Epistemic>,
        /// §6.4 provenance chain: AIDs of who asserted/forwarded, oldest first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Vec<String>>,
        /// §6.4 confidence in [0,1].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f32>,
        /// §6.4 temporal validity: fact expires at this unix ms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until_ms: Option<i64>,
    },
    /// §6.3 conjunction — all leaves must hold.
    #[serde(rename = "and")]
    And { exprs: Vec<ClExpr> },
    /// §6.3 disjunction — at least one leaf must hold.
    #[serde(rename = "or")]
    Or { exprs: Vec<ClExpr> },
    /// §6.3 negation (fact leaves only).
    #[serde(rename = "not")]
    Not { expr: Box<ClExpr> },
    /// §6.3 reference to an expression produced in another message.
    #[serde(rename = "ref")]
    RefExpr { msg_id: String },
    /// Express an action to be performed.
    #[serde(rename = "action")]
    Action {
        name: String,
        #[serde(default)]
        params: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_schema: Option<serde_json::Value>,
    },
    /// Ask whether `predicate(args)` holds; the reply is a `Fact`.
    #[serde(rename = "query")]
    Query {
        predicate: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    /// Debtor commits to creditor to perform `act` by `deadline_ms`.
    #[serde(rename = "commitment")]
    Commitment {
        debtor: String,
        creditor: String,
        act: ClAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<i64>,
        status: CommitmentStatus,
    },
}

fn default_true() -> bool {
    true
}

impl ClExpr {
    pub fn fact(predicate: &str, args: serde_json::Value) -> Self {
        ClExpr::Fact {
            predicate: predicate.into(),
            args: json_object_to_btree(args),
            truth: true,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }
    }

    pub fn fact_false(predicate: &str, args: serde_json::Value) -> Self {
        ClExpr::Fact {
            predicate: predicate.into(),
            args: json_object_to_btree(args),
            truth: false,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }
    }

    pub fn action(name: &str, params: serde_json::Value) -> Self {
        ClExpr::Action {
            name: name.into(),
            params,
            result_schema: None,
        }
    }

    pub fn and(exprs: Vec<ClExpr>) -> Self {
        ClExpr::And { exprs }
    }

    pub fn not(expr: ClExpr) -> Self {
        ClExpr::Not { expr: Box::new(expr) }
    }

    pub fn ref_to(msg_id: &str) -> Self {
        ClExpr::RefExpr { msg_id: msg_id.into() }
    }

    pub fn query(predicate: &str, args: serde_json::Value) -> Self {
        ClExpr::Query {
            predicate: predicate.into(),
            args,
        }
    }

    pub fn commitment(
        debtor: &str,
        creditor: &str,
        act: ClAction,
        deadline_ms: Option<i64>,
        status: CommitmentStatus,
    ) -> Self {
        ClExpr::Commitment {
            debtor: debtor.into(),
            creditor: creditor.into(),
            act,
            deadline_ms,
            status,
        }
    }
}

fn json_object_to_btree(v: serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    v.as_object()
        .map(|m| m.iter().map(|(k, val)| (k.clone(), val.clone())).collect())
        .unwrap_or_default()
}

use crate::Performative;

/// Normative grammar (spec §6.2): which CL ops each performative may carry.
/// Returns Err with a human-readable reason on ill-formed combinations.
pub fn well_formed(performative: Performative, exprs: &[ClExpr]) -> Result<(), String> {
    if exprs.is_empty() {
        return Err("Cl body with zero expressions".into());
    }
    let allowed = match performative {
        Performative::Inform => "fact",
        Performative::Request => "action",
        Performative::Query => "query",
        Performative::Propose => "commitment",
        Performative::Accept => "commitment",
        Performative::Delegate => "action",
        other => {
            return Err(format!(
                "performative {other:?} does not carry a Cl body (has its own variant)"
            ))
        }
    };
    for e in exprs {
        check_leaf(performative, allowed, e)?;
    }
    Ok(())
}

/// Recursively validate one expression (§6.3: composition composes
/// the constraint down to leaves; `not` wraps fact leaves; `ref` is
/// neutral — it binds to another message whose grammar was checked there).
fn check_leaf(performative: Performative, allowed: &str, e: &ClExpr) -> Result<(), String> {
    match e {
        ClExpr::And { exprs } | ClExpr::Or { exprs } => {
            if exprs.is_empty() {
                return Err("empty composition".into());
            }
            for sub in exprs {
                check_leaf(performative, allowed, sub)?;
            }
            Ok(())
        }
        ClExpr::Not { expr } => match expr.as_ref() {
            ClExpr::Fact { .. } => Ok(()),
            other => Err(format!("'not' may only wrap a fact, got {other:?}")),
        },
        ClExpr::RefExpr { msg_id } => {
            if msg_id.is_empty() {
                return Err("ref with empty msg_id".into());
            }
            Ok(())
        }
        ClExpr::Fact { .. } => {
            if allowed != "fact" {
                return Err(format!(
                    "performative {performative:?} carries CL op 'fact' but allows only '{allowed}'"
                ));
            }
            Ok(())
        }
        ClExpr::Action { .. } => {
            if allowed != "action" {
                return Err(format!(
                    "performative {performative:?} carries CL op 'action' but allows only '{allowed}'"
                ));
            }
            Ok(())
        }
        ClExpr::Query { .. } => {
            if allowed != "query" {
                return Err(format!(
                    "performative {performative:?} carries CL op 'query' but allows only '{allowed}'"
                ));
            }
            Ok(())
        }
        ClExpr::Commitment { status, .. } => {
            let ok = match performative {
                Performative::Propose => *status == CommitmentStatus::Proposed,
                Performative::Accept => *status == CommitmentStatus::Active,
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "performative {performative:?} cannot carry commitment with status {status:?}"
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aid, Body, Envelope};

    fn env(perf: Performative, body: Body) -> Envelope {
        Envelope::new(Aid::local(1, "t"), Aid::new("did:x:y"), perf, body)
    }

    fn cl_body(exprs: Vec<ClExpr>) -> Body {
        Body::Cl { expressions: exprs }
    }

    #[test]
    fn roundtrip_all_ops() {
        let exprs = vec![
            ClExpr::fact("file_exists", serde_json::json!({"path": "/tmp/x"})),
            ClExpr::fact_false("agent_healthy", serde_json::json!({"agent": "argus"})),
            ClExpr::action("text.reverse", serde_json::json!({"text": "abc"})),
            ClExpr::query("task_status", serde_json::json!({"msg_id": "abc"})),
            ClExpr::commitment(
                "local:1:kenji",
                "local:1:argus",
                ClAction {
                    name: "text.reverse".into(),
                    params: serde_json::json!({"text": "abc"}),
                    result_schema: None,
                },
                Some(1_700_000_000_000),
                CommitmentStatus::Proposed,
            ),
        ];
        let env = env(Performative::Inform, cl_body(exprs.clone()));
        let bytes = env.encode().unwrap();
        let back = Envelope::decode(&bytes).unwrap();
        match &back.body {
            Body::Cl { expressions } => assert_eq!(expressions, &exprs),
            other => panic!("expected Cl, got {other:?}"),
        }
    }

    #[test]
    fn canonical_serialization_order_independent() {
        // same fact, different insertion order -> identical bytes
        let mut e1 = env(Performative::Inform, cl_body(vec![ClExpr::Fact {
            predicate: "f".into(),
            args: BTreeMap::from([("a".into(), serde_json::json!(1)), ("b".into(), serde_json::json!(2))]),
            truth: true,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }]));
        let mut e2 = env(Performative::Inform, cl_body(vec![ClExpr::Fact {
            predicate: "f".into(),
            args: BTreeMap::from([("b".into(), serde_json::json!(2)), ("a".into(), serde_json::json!(1))]),
            truth: true,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }]));
        // pin msg_id so the only difference under test is arg ordering
        e1.msg_id = "canonical-test-id-0001".into();
        e2.msg_id = "canonical-test-id-0001".into();
        e1.ts = 0;
        e2.ts = 0;
        assert_eq!(e1.encode().unwrap(), e2.encode().unwrap());
    }

    #[test]
    fn grammar_matrix() {
        // well-formed combinations
        assert!(well_formed(Performative::Inform, &[ClExpr::fact("f", serde_json::json!({}))]).is_ok());
        assert!(well_formed(Performative::Inform, &[ClExpr::fact_false("f", serde_json::json!({}))]).is_ok());
        assert!(well_formed(Performative::Request, &[ClExpr::action("t.reverse", serde_json::json!({}))]).is_ok());
        assert!(well_formed(Performative::Query, &[ClExpr::query("f", serde_json::json!({}))]).is_ok());
        assert!(well_formed(Performative::Delegate, &[ClExpr::action("t", serde_json::json!({}))]).is_ok());
        assert!(well_formed(
            Performative::Propose,
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None }, None, CommitmentStatus::Proposed)]
        ).is_ok());
        assert!(well_formed(
            Performative::Accept,
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None }, None, CommitmentStatus::Active)]
        ).is_ok());

        // ill-formed combinations
        assert!(well_formed(Performative::Inform, &[ClExpr::action("t", serde_json::json!({}))]).is_err());
        assert!(well_formed(Performative::Request, &[ClExpr::fact("f", serde_json::json!({}))]).is_err());
        assert!(well_formed(Performative::Query, &[ClExpr::action("t", serde_json::json!({}))]).is_err());
        // commitment status must match performative
        assert!(well_formed(
            Performative::Propose,
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None }, None, CommitmentStatus::Active)]
        ).is_err());
        // performatives with their own body variants never carry CL
        assert!(well_formed(Performative::Progress, &[ClExpr::fact("f", serde_json::json!({}))]).is_err());
        // empty CL body
        assert!(well_formed(Performative::Inform, &[]).is_err());
    }

    #[test]
    fn commitment_lifecycle() {
        use CommitmentStatus::*;
        assert!(Proposed.can_transition(Active));
        assert!(Proposed.can_transition(Cancelled));
        assert!(Active.can_transition(Fulfilled));
        assert!(Active.can_transition(Breached));
        assert!(Active.can_transition(Cancelled));
        // illegal
        assert!(!Proposed.can_transition(Fulfilled));
        assert!(!Proposed.can_transition(Breached));
        assert!(!Fulfilled.can_transition(Active));
        assert!(!Breached.can_transition(Active));
        assert!(!Cancelled.can_transition(Active));
        assert!(!Fulfilled.can_transition(Fulfilled)); // terminal
    }

    #[test]
    fn composition_and_epistemic() {
        use Epistemic::*;
        // and/or of facts under inform: well-formed
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::and(vec![
                ClExpr::fact("a", serde_json::json!({})),
                ClExpr::fact("b", serde_json::json!({})),
            ])]
        ).is_ok());
        // nested composition
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::Or { exprs: vec![
                ClExpr::and(vec![ClExpr::fact("a", serde_json::json!({}))]),
                ClExpr::not(ClExpr::fact("b", serde_json::json!({}))),
            ]}]
        ).is_ok());
        // composition must still respect the matrix: and(action) under inform
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::and(vec![ClExpr::action("t", serde_json::json!({}))])]
        ).is_err());
        // not() only wraps facts
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::not(ClExpr::action("t", serde_json::json!({})))]
        ).is_err());
        // ref is neutral, even under request... no: ref allowed as leaf
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::ref_to("msg123"), ClExpr::fact("f", serde_json::json!({}))]
        ).is_ok());
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::ref_to("")]
        ).is_err());

        // epistemic fact roundtrip
        let f = ClExpr::Fact {
            predicate: "file_exists".into(),
            args: BTreeMap::from([("path".into(), serde_json::json!("/tmp/x"))]),
            truth: true,
            modality: Some(Believed),
            provenance: Some(vec!["did:example:a".into(), "did:example:b".into()]),
            confidence: Some(0.8),
            valid_until_ms: Some(1_700_000_000_000),
        };
        let fenv = env(Performative::Inform, cl_body(vec![f]));
        let back = Envelope::decode(&fenv.encode().unwrap()).unwrap();
        match &back.body {
            Body::Cl { expressions } => match &expressions[0] {
                ClExpr::Fact { modality, provenance, confidence, valid_until_ms, .. } => {
                    assert_eq!(*modality, Some(Believed));
                    assert_eq!(provenance.as_ref().unwrap().len(), 2);
                    assert_eq!(*confidence, Some(0.8));
                    assert_eq!(*valid_until_ms, Some(1_700_000_000_000));
                }
                other => panic!("expected Fact, got {other:?}"),
            },
            other => panic!("expected Cl, got {other:?}"),
        }
        // absent fields decode as None (backward compatible with v0.2 facts)
        let plain = env(Performative::Inform, cl_body(vec![ClExpr::fact("f", serde_json::json!({}))]));
        match Envelope::decode(&plain.encode().unwrap()).unwrap().body {
            Body::Cl { expressions } => match expressions[0] {
                ClExpr::Fact { modality: None, provenance: None, confidence: None, valid_until_ms: None, .. } => {}
                _ => panic!("defaults must be None"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn mixed_ops_rejected() {
        // one message carrying fact + action is ill-formed under inform
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::fact("f", serde_json::json!({})), ClExpr::action("t", serde_json::json!({}))]
        )
        .is_err());
    }
}
