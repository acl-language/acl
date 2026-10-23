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
    },
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
        }
    }

    pub fn fact_false(predicate: &str, args: serde_json::Value) -> Self {
        ClExpr::Fact {
            predicate: predicate.into(),
            args: json_object_to_btree(args),
            truth: false,
        }
    }

    pub fn action(name: &str, params: serde_json::Value) -> Self {
        ClExpr::Action {
            name: name.into(),
            params,
            result_schema: None,
        }
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
        let op = match e {
            ClExpr::Fact { .. } => "fact",
            ClExpr::Action { .. } => "action",
            ClExpr::Query { .. } => "query",
            ClExpr::Commitment { status, .. } => {
                // commitments: performative must agree with status
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
                continue;
            }
        };
        if op != allowed {
            return Err(format!(
                "performative {performative:?} carries CL op '{op}' but allows only '{allowed}'"
            ));
        }
    }
    Ok(())
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
        }]));
        let mut e2 = env(Performative::Inform, cl_body(vec![ClExpr::Fact {
            predicate: "f".into(),
            args: BTreeMap::from([("b".into(), serde_json::json!(2)), ("a".into(), serde_json::json!(1))]),
            truth: true,
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
    fn mixed_ops_rejected() {
        // one message carrying fact + action is ill-formed under inform
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::fact("f", serde_json::json!({})), ClExpr::action("t", serde_json::json!({}))]
        )
        .is_err());
    }
}
