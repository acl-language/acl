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

/// Typed content value (§27): the CL's own type system. Wire-compatible
/// with plain msgpack/JSON values (Str→string, Int→integer, …) so existing
/// encoders keep working — but Rust-level code now knows the TYPE.
/// `Var` is the exception: it serializes as a single-key map `{"$var":
/// name}` to stay unambiguous with `Str`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// §28: reference to a `Bind` binding or a quantified variable.
    Var(String),
    List(Vec<ClValue>),
    Map(BTreeMap<String, ClValue>),
    Bytes(Vec<u8>),
}

impl ClValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            ClValue::Str(_) => "string",
            ClValue::Int(_) => "int",
            ClValue::Float(_) => "float",
            ClValue::Bool(_) => "bool",
            ClValue::Var(_) => "var",
            ClValue::List(_) => "list",
            ClValue::Map(_) => "map",
            ClValue::Bytes(_) => "bytes",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self { ClValue::Str(s) => Some(s), _ => None }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self { ClValue::Int(i) => Some(*i), _ => None }
    }

    /// Strict conversion from untyped JSON: int rejects floats, float
    /// accepts any number, objects/arrays recurse. `null` is rejected —
    /// CL values must be typed.
    pub fn from_json(v: serde_json::Value) -> Result<Self, String> {
        Ok(match v {
            serde_json::Value::String(s) => ClValue::Str(s),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    ClValue::Int(i)
                } else {
                    ClValue::Float(n.as_f64().ok_or("unrepresentable number")?)
                }
            }
            serde_json::Value::Bool(b) => ClValue::Bool(b),
            serde_json::Value::Array(a) => {
                ClValue::List(a.into_iter().map(ClValue::from_json).collect::<Result<Vec<_>, _>>()?)
            }
            serde_json::Value::Object(o) => ClValue::Map(
                o.into_iter()
                    .map(|(k, v)| ClValue::from_json(v).map(|cv| (k, cv)))
                    .collect::<Result<BTreeMap<_, _>, _>>()?,
            ),
            serde_json::Value::Null => return Err("null is not a CL value".into()),
        })
    }
}

impl Serialize for ClValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ClValue::Str(s) => serializer.serialize_str(s),
            ClValue::Int(i) => serializer.serialize_i64(*i),
            ClValue::Float(f) => serializer.serialize_f64(*f),
            ClValue::Bool(b) => serializer.serialize_bool(*b),
            ClValue::Var(name) => {
                use serde::ser::SerializeMap;
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("$var", name)?;
                m.end()
            }
            ClValue::List(l) => serializer.serialize_some(l),
            ClValue::Map(m) => {
                use serde::ser::SerializeMap;
                let mut sm = serializer.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    sm.serialize_entry(k, v)?;
                }
                sm.end()
            }
            ClValue::Bytes(b) => serializer.serialize_bytes(b),
        }
    }
}

impl<'de> Deserialize<'de> for ClValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        // map with single key "$var" → Var (checked before Map)
        if let serde_json::Value::Object(ref o) = v {
            if o.len() == 1 {
                if let Some(name) = o.get("$var").and_then(|x| x.as_str()) {
                    return Ok(ClValue::Var(name.to_string()));
                }
            }
        }
        ClValue::from_json(v).map_err(serde::de::Error::custom)
    }
}

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
    /// §28: params deliberately left unfilled — the action is a template
    /// awaiting completion. Schema validation skips these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<Vec<String>>,
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
        args: BTreeMap<String, ClValue>,
        #[serde(default = "default_true")]
        truth: bool,
        /// §29: this fact was derived from the referenced message; its
        /// validity is bounded by the source's validity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        derived_from: Option<String>,
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
    /// §28: bind a name to an expression's value, usable in `body` via
    /// `$bind:<name>` references. `expr` must be a fact/action leaf.
    #[serde(rename = "bind")]
    Bind { name: String, expr: Box<ClExpr>, body: Box<ClExpr> },
    /// §28: universal quantification — for every element of `over`'s
    /// result (a query), `such_that` holds with the element bound to `var`.
    #[serde(rename = "for_all")]
    ForAll { var: String, over: Box<ClExpr>, such_that: Box<ClExpr> },
    /// §28: existential quantification.
    #[serde(rename = "exists")]
    Exists { var: String, over: Box<ClExpr>, such_that: Box<ClExpr> },
    /// §28: metanível — the sender asserts that `agent` holds `fact`.
    #[serde(rename = "believes")]
    Believes { agent: String, fact: Box<ClExpr> },
    /// §29: extend an active commitment's deadline (carried under request).
    #[serde(rename = "renew")]
    Renew { target_msg_id: String, new_deadline_ms: i64 },
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
        /// §29: the asker accepts answers valid until this unix ms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until_ms: Option<i64>,
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
            derived_from: None,
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
            derived_from: None,
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

    pub fn bind(name: &str, expr: ClExpr, body: ClExpr) -> Self {
        ClExpr::Bind { name: name.into(), expr: Box::new(expr), body: Box::new(body) }
    }

    pub fn for_all(var: &str, over: ClExpr, such_that: ClExpr) -> Self {
        ClExpr::ForAll { var: var.into(), over: Box::new(over), such_that: Box::new(such_that) }
    }

    pub fn exists(var: &str, over: ClExpr, such_that: ClExpr) -> Self {
        ClExpr::Exists { var: var.into(), over: Box::new(over), such_that: Box::new(such_that) }
    }

    pub fn believes(agent: &str, fact: ClExpr) -> Self {
        ClExpr::Believes { agent: agent.into(), fact: Box::new(fact) }
    }

    pub fn renew(target_msg_id: &str, new_deadline_ms: i64) -> Self {
        ClExpr::Renew { target_msg_id: target_msg_id.into(), new_deadline_ms }
    }

    pub fn query(predicate: &str, args: serde_json::Value) -> Self {
        ClExpr::Query {
            predicate: predicate.into(),
            args,
            valid_until_ms: None,
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

fn json_object_to_btree(v: serde_json::Value) -> BTreeMap<String, ClValue> {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, val)| ClValue::from_json(val.clone()).map(|cv| (k.clone(), cv)))
                .collect::<Result<BTreeMap<_, _>, _>>()
                .unwrap_or_default()
        })
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
    let mut bound = Vec::new();
    for e in exprs {
        check_leaf(performative, allowed, e, &mut bound)?;
    }
    Ok(())
}

/// Recursively validate one expression (§6.3/§6.5): composition composes
/// the constraint down to leaves; `not` wraps fact leaves; `ref` is
/// neutral; `bind` extends the lexical scope; quantifiers bind their var.
fn check_leaf(
    performative: Performative,
    allowed: &str,
    e: &ClExpr,
    bound: &mut Vec<String>,
) -> Result<(), String> {
    match e {
        ClExpr::And { exprs } | ClExpr::Or { exprs } => {
            if exprs.is_empty() {
                return Err("empty composition".into());
            }
            for sub in exprs {
                check_leaf(performative, allowed, sub, bound)?;
            }
            Ok(())
        }
        ClExpr::Not { expr } => match expr.as_ref() {
            ClExpr::Fact { .. } => Ok(()),
            other => Err(format!("'not' may only wrap a fact, got {other:?}")),
        },
        // (scope flows through all composition arms below via check_leaf calls)
        ClExpr::RefExpr { msg_id } => {
            if msg_id.is_empty() {
                return Err("ref with empty msg_id".into());
            }
            // $bind:<name> references must have an enclosing binding (§28)
            if let Some(name) = msg_id.strip_prefix("$bind:") {
                if !bound.iter().any(|b| b == name) {
                    return Err(format!("$bind:{name} references an unbound name"));
                }
            }
            Ok(())
        }
        ClExpr::Bind { name, expr, body } => {
            // §28: the bound expression must be a leaf (fact or action)
            match expr.as_ref() {
                ClExpr::Fact { .. } | ClExpr::Action { .. } => {}
                other => return Err(format!("bind '{name}' must bind a fact or action leaf, got {other:?}")),
            }
            check_leaf(performative, allowed, expr, bound)?;
            bound.push(name.clone());
            let r = check_leaf(performative, allowed, body, bound);
            bound.pop();
            r
        }
        ClExpr::ForAll { var, over, such_that }
        | ClExpr::Exists { var, over, such_that } => {
            // §28: the domain must be a query (its result is the collection)
            if !matches!(over.as_ref(), ClExpr::Query { .. }) {
                return Err(format!("quantifier 'over' must be a query, got {over:?}"));
            }
            check_leaf(performative, "query", over, bound)?;
            bound.push(var.clone());
            let r = check_leaf(performative, allowed, such_that, bound);
            bound.pop();
            r
        }
        ClExpr::Believes { agent, fact } => {
            // §28: metanível — only under inform; the inner fact is validated
            if performative != Performative::Inform {
                return Err(format!("believes is only legal under inform, got {performative:?}"));
            }
            if agent.is_empty() {
                return Err("believes with empty agent".into());
            }
            check_leaf(performative, allowed, fact, bound)
        }
        ClExpr::Renew { target_msg_id, new_deadline_ms } => {
            // §29: only under request; target must be set, deadline in future
            if performative != Performative::Request {
                return Err(format!("renew is only legal under request, got {performative:?}"));
            }
            if target_msg_id.is_empty() {
                return Err("renew with empty target".into());
            }
            if *new_deadline_ms <= 0 {
                return Err("renew deadline must be positive".into());
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
                    partial: None,
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
            args: BTreeMap::from([
                ("a".into(), ClValue::Int(1)),
                ("b".into(), ClValue::Int(2)),
            ]),
            truth: true,
            derived_from: None,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        }]));
        let mut e2 = env(Performative::Inform, cl_body(vec![ClExpr::Fact {
            predicate: "f".into(),
            args: BTreeMap::from([
                ("b".into(), ClValue::Int(2)),
                ("a".into(), ClValue::Int(1)),
            ]),
            truth: true,
            derived_from: None,
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
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None, partial: None }, None, CommitmentStatus::Proposed)]
        ).is_ok());
        assert!(well_formed(
            Performative::Accept,
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None, partial: None }, None, CommitmentStatus::Active)]
        ).is_ok());

        // ill-formed combinations
        assert!(well_formed(Performative::Inform, &[ClExpr::action("t", serde_json::json!({}))]).is_err());
        assert!(well_formed(Performative::Request, &[ClExpr::fact("f", serde_json::json!({}))]).is_err());
        assert!(well_formed(Performative::Query, &[ClExpr::action("t", serde_json::json!({}))]).is_err());
        // commitment status must match performative
        assert!(well_formed(
            Performative::Propose,
            &[ClExpr::commitment("a", "b", ClAction { name: "t".into(), params: serde_json::json!({}), result_schema: None, partial: None }, None, CommitmentStatus::Active)]
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
            args: BTreeMap::from([("path".into(), ClValue::Str("/tmp/x".into()))]),
            truth: true,
            derived_from: None,
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
    fn clvalue_types_and_wire_compat() {
        use ClValue::*;
        // wire compatibility: typed values serialize identically to plain
        let f_typed = ClExpr::Fact {
            predicate: "p".into(),
            args: BTreeMap::from([
                ("s".into(), Str("x".into())),
                ("i".into(), Int(42)),
                ("f".into(), Float(1.5)),
                ("b".into(), Bool(true)),
                ("l".into(), List(vec![Int(1), Int(2)])),
            ]),
            truth: true,
            derived_from: None,
            modality: None,
            provenance: None,
            confidence: None,
            valid_until_ms: None,
        };
        let f_plain = ClExpr::fact("p", serde_json::json!({
            "s": "x", "i": 42, "f": 1.5, "b": true, "l": [1, 2]
        }));
        let mut e_typed = env(Performative::Inform, cl_body(vec![f_typed]));
        let mut e_plain = env(Performative::Inform, cl_body(vec![f_plain]));
        e_typed.msg_id = "typed-plain-0001".into();
        e_plain.msg_id = "typed-plain-0001".into();
        e_typed.ts = 0;
        e_plain.ts = 0;
        assert_eq!(e_typed.encode().unwrap(), e_plain.encode().unwrap(),
            "typed and plain must be wire-identical");

        // strict typing: from_json rejects int-as-float
        assert!(ClValue::from_json(serde_json::json!(5.5)).is_ok()); // Float
        assert!(matches!(ClValue::from_json(serde_json::json!(5)), Ok(ClValue::Int(5))));
        assert!(ClValue::from_json(serde_json::json!(null)).is_err());
        // type_name
        assert_eq!(ClValue::Str("x".into()).type_name(), "string");
        assert_eq!(ClValue::Int(1).type_name(), "int");
    }

    #[test]
    fn bind_scope_and_partial() {
        // $bind reference inside body: legal
        let bound = ClExpr::bind(
            "f",
            ClExpr::fact("file_exists", serde_json::json!({"path": "/x"})),
            ClExpr::ref_to("$bind:f"),
        );
        assert!(well_formed(Performative::Inform, &[bound]).is_ok());
        // $bind reference WITHOUT enclosing bind: illegal
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::ref_to("$bind:missing")]
        ).is_err());
        // bind must wrap a leaf
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::bind(
                "f",
                ClExpr::and(vec![ClExpr::fact("a", serde_json::json!({}))]),
                ClExpr::ref_to("$bind:f"),
            )]
        ).is_err());
    }

    #[test]
    fn quantification_and_metanivel() {
        // for_all with query domain: legal under inform
        let fa = ClExpr::for_all(
            "f",
            ClExpr::query("dir_entries", serde_json::json!({"path": "/x"})),
            ClExpr::fact("file_exists", serde_json::json!({"path": "$var:f"})),
        );
        assert!(well_formed(Performative::Inform, &[fa]).is_ok());
        // over must be a query
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::for_all(
                "f",
                ClExpr::fact("x", serde_json::json!({})),
                ClExpr::fact("y", serde_json::json!({})),
            )]
        ).is_err());
        // believes: only under inform, wraps fact
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::believes(
                "did:key:zabc",
                ClExpr::fact("healthy", serde_json::json!({"agent": "x"})),
            )]
        ).is_ok());
        assert!(well_formed(
            Performative::Request,
            &[ClExpr::believes("a", ClExpr::fact("f", serde_json::json!({})))]
        ).is_err());
        // renew: only under request
        assert!(well_formed(
            Performative::Request,
            &[ClExpr::renew("commitment-1", 1700000000000)]
        ).is_ok());
        assert!(well_formed(
            Performative::Inform,
            &[ClExpr::renew("c", 1700000000000)]
        ).is_err());
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
