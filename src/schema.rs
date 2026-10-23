//! Runtime schema validation (spec §19): validate incoming CL payloads
//! against a compiled IDL schema BEFORE processing.
//!
//! Build time: `aclc gen schema vocab.acl` emits `<ns>.schema.json`.
//! Runtime: `RuntimeSchema::from_json` loads it; `validate()` checks a
//! `Body::Cl` payload against the declared vocabulary. This closes the
//! loop started in §18 — types are enforced at build time AND on the wire.

use crate::cl::ClExpr;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaOp {
    /// param name -> primitive or alias type
    pub params: BTreeMap<String, String>,
    pub ret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSchema {
    pub namespace: String,
    pub version: u32,
    /// alias -> primitive (resolved at compile time)
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// local name -> op (full name = namespace.local)
    pub predicates: BTreeMap<String, SchemaOp>,
    pub actions: BTreeMap<String, SchemaOp>,
    /// §31: deprecated terms (full name -> reason)
    #[serde(default)]
    pub deprecated: BTreeMap<String, String>,
}

impl RuntimeSchema {
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("bad schema json: {e}"))
    }

    /// Resolve a type reference (alias or primitive) to its primitive.
    fn resolve<'a>(&'a self, ty: &'a str) -> Result<&'a str, String> {
        if crate::idl::PRIMITIVES.contains(&ty) {
            Ok(ty)
        } else {
            self.aliases
                .get(ty)
                .map(|s| s.as_str())
                .ok_or_else(|| format!("unknown type '{ty}'"))
        }
    }

    /// Check one JSON value against a declared type. Rules:
    /// - string  -> is_string
    /// - int     -> integer (not float)
    /// - float   -> any number
    /// - bool    -> is_boolean
    /// - bytes   -> base64/hex string or array (lenient: any string)
    /// - timestamp / any -> int / anything
    fn check_value(&self, ty: &str, v: &serde_json::Value) -> Result<(), String> {
        let prim = self.resolve(ty)?;
        let ok = match prim {
            "string" => v.is_string(),
            "int" | "timestamp" => v.as_i64().is_some() || v.as_u64().is_some(),
            "float" => v.is_number(),
            "bool" => v.is_boolean(),
            "bytes" => v.is_string() || v.is_array(), // encoded form
            "any" => true,
            other => return Err(format!("unresolved primitive '{other}'")),
        };
        if ok {
            Ok(())
        } else {
            Err(format!("expected {prim}, got {}", type_of(v)))
        }
    }

    fn check_op(
        &self,
        kind: &str,
        table: &BTreeMap<String, SchemaOp>,
        full_name: &str,
        params: &serde_json::Value,
    ) -> Result<(), String> {
        let local = full_name
            .strip_prefix(&format!("{}.", self.namespace))
            .ok_or_else(|| format!("{kind} '{full_name}' is not in namespace '{}'", self.namespace))?;
        let op = table
            .get(local)
            .ok_or_else(|| format!("unknown {kind} '{full_name}'"))?;
        let obj = params
            .as_object()
            .ok_or_else(|| format!("{kind} '{full_name}' params must be an object"))?;
        // declared params: present + typed
        for (pname, pty) in &op.params {
            let v = obj
                .get(pname)
                .ok_or_else(|| format!("{kind} '{full_name}' missing param '{pname}'"))?;
            self.check_value(pty, v).map_err(|e| format!("{kind} '{full_name}' param '{pname}': {e}"))?;
        }
        // strict: undeclared params rejected
        for k in obj.keys() {
            if !op.params.contains_key(k) {
                return Err(format!("{kind} '{full_name}' has undeclared param '{k}'"));
            }
        }
        Ok(())
    }

    /// Validate a CL expression (recursively, §6.3) against the schema.
    /// `RefExpr` is skipped (it binds to another message's validation).
    pub fn validate(&self, e: &ClExpr) -> Result<(), String> {
        match e {
            ClExpr::Fact { predicate, args, .. } => {
                let args_val = serde_json::to_value(args).unwrap_or(serde_json::Value::Null);
                self.check_op("predicate", &self.predicates, predicate, &args_val)
            }
            ClExpr::Action { name, params, .. } => {
                self.check_op("action", &self.actions, name, params)
            }
            ClExpr::Query { predicate, args, .. } => {
                self.check_op("predicate", &self.predicates, predicate, args)
            }
            ClExpr::Commitment { act, .. } => self.validate(&ClExpr::Action {
                name: act.name.clone(),
                params: act.params.clone(),
                result_schema: act.result_schema.clone(),
            }),
            ClExpr::And { exprs } | ClExpr::Or { exprs } => {
                for sub in exprs {
                    self.validate(sub)?;
                }
                Ok(())
            }
            ClExpr::Not { expr } => self.validate(expr),
            ClExpr::RefExpr { .. } => Ok(()), // validated where it was produced
            ClExpr::Bind { expr, body, .. } => {
                self.validate(expr)?;
                self.validate(body)
            }
            ClExpr::ForAll { over, such_that, .. }
            | ClExpr::Exists { over, such_that, .. } => {
                self.validate(over)?;
                self.validate(such_that)
            }
            ClExpr::Believes { fact, .. } => self.validate(fact),
            // §29: renew targets a prior message — nothing to validate here
            ClExpr::Renew { .. } => Ok(()),
            // §32: conditional commitment validates its condition + act
            ClExpr::ConditionalCommitment { condition, act, .. } => {
                self.validate(condition)?;
                self.validate(&ClExpr::Action {
                    name: act.name.clone(),
                    params: act.params.clone(),
                    result_schema: act.result_schema.clone(),
                })
            }
            // §32: preference criteria are policy facts — validated
            ClExpr::Prefer { criteria } => {
                for c in criteria {
                    self.validate(c)?;
                }
                Ok(())
            }
        }
    }

    /// §31: validate and COLLECT deprecation warnings. Returns
    /// (validation_result, warnings). Warnings don't fail validation —
    /// deprecated terms still work until the vocabulary's major bump.
    pub fn validate_with_warnings(
        &self,
        exprs: &[ClExpr],
    ) -> (Result<(), Vec<String>>, Vec<String>) {
        let result = self.validate_all(exprs);
        let mut warnings = Vec::new();
        for e in exprs {
            collect_dep_warnings(e, &self.deprecated, &self.namespace, &mut warnings);
        }
        (result, warnings)
    }

    /// Validate every expression of a Cl body.
    pub fn validate_all(&self, exprs: &[ClExpr]) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for e in exprs {
            if let Err(msg) = self.validate(e) {
                errors.push(msg);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Walk expressions collecting deprecation warnings for used terms.
fn collect_dep_warnings(
    e: &ClExpr,
    deprecated: &BTreeMap<String, String>,
    namespace: &str,
    out: &mut Vec<String>,
) {
    fn check_term(term: &str, deprecated: &BTreeMap<String, String>, out: &mut Vec<String>) {
        if let Some(reason) = deprecated.get(term) {
            out.push(format!("{term} is deprecated: {reason}"));
        }
    }
    match e {
        ClExpr::Fact { predicate, .. } => check_term(predicate, deprecated, out),
        ClExpr::Action { name, .. } => check_term(name, deprecated, out),
        ClExpr::Query { predicate, .. } => check_term(predicate, deprecated, out),
        ClExpr::Commitment { act, .. } => check_term(&act.name, deprecated, out),
        ClExpr::And { exprs } | ClExpr::Or { exprs } => {
            for sub in exprs {
                collect_dep_warnings(sub, deprecated, namespace, out);
            }
        }
        ClExpr::Not { expr } | ClExpr::Bind { body: expr, .. } => {
            collect_dep_warnings(expr, deprecated, namespace, out)
        }
        ClExpr::ForAll { over, such_that, .. }
        | ClExpr::Exists { over, such_that, .. } => {
            collect_dep_warnings(over, deprecated, namespace, out);
            collect_dep_warnings(such_that, deprecated, namespace, out);
        }
        ClExpr::Believes { fact, .. } => {
            collect_dep_warnings(fact, deprecated, namespace, out)
        }
        ClExpr::ConditionalCommitment { condition, act, .. } => {
            collect_dep_warnings(condition, deprecated, namespace, out);
            check_term(&act.name, deprecated, out);
        }
        ClExpr::Prefer { criteria } => {
            for c in criteria {
                collect_dep_warnings(c, deprecated, namespace, out);
            }
        }
        ClExpr::Renew { .. } | ClExpr::RefExpr { .. } => {}
    }
}

fn type_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cl::ClAction;
    use std::collections::BTreeMap;

    fn schema() -> RuntimeSchema {
        RuntimeSchema {
            namespace: "acl.std.fs".into(),
            version: 1,
            aliases: BTreeMap::from([("Path".to_string(), "string".to_string())]),
            predicates: BTreeMap::from([(
                "file_exists".to_string(),
                SchemaOp {
                    params: BTreeMap::from([("path".to_string(), "Path".to_string())]),
                    ret: "bool".into(),
                    doc: None,
                },
            )]),
            actions: BTreeMap::from([(
                "file_read".to_string(),
                SchemaOp {
                    params: BTreeMap::from([("path".to_string(), "Path".to_string())]),
                    ret: "string".into(),
                    doc: None,
                },
            )]),
            deprecated: BTreeMap::new(),
        }
    }

    #[test]
    fn valid_fact_and_action() {
        let s = schema();
        assert!(s
            .validate(&ClExpr::fact(
                "acl.std.fs.file_exists",
                serde_json::json!({"path": "/tmp/x"})
            ))
            .is_ok());
        assert!(s
            .validate(&ClExpr::Action {
                name: "acl.std.fs.file_read".into(),
                params: serde_json::json!({"path": "/tmp/x"}),
                result_schema: None,
            })
            .is_ok());
    }

    #[test]
    fn missing_param_wrong_type_extra_param() {
        let s = schema();
        // missing param
        assert!(s.validate(&ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({}))).is_err());
        // wrong type (int for Path)
        assert!(s
            .validate(&ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({"path": 42})))
            .is_err());
        // extra undeclared param
        assert!(s
            .validate(&ClExpr::fact(
                "acl.std.fs.file_exists",
                serde_json::json!({"path": "/x", "mode": "rw"})
            ))
            .is_err());
        // unknown predicate
        assert!(s.validate(&ClExpr::fact("acl.std.fs.nothing", serde_json::json!({}))).is_err());
        // foreign namespace
        assert!(s.validate(&ClExpr::fact("other.ns.f", serde_json::json!({}))).is_err());
    }

    #[test]
    fn alias_and_composition_recursion() {
        let s = schema();
        // 'Path' alias resolves to string — value must be a string
        assert!(s.validate(&ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({"path": "ok"}))).is_ok());
        // composition recursion: valid inside and()
        assert!(s
            .validate(&ClExpr::and(vec![
                ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({"path": "/x"})),
                ClExpr::not(ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({"path": "/y"}))),
            ]))
            .is_ok());
        // invalid leaf inside and() surfaces
        let errs = s
            .validate_all(&[ClExpr::and(vec![ClExpr::fact(
                "acl.std.fs.file_exists",
                serde_json::json!({"path": 99}),
            )])])
            .unwrap_err();
        assert!(errs[0].contains("expected string"));
    }

    #[test]
    fn commitment_validates_its_act() {
        let s = schema();
        let c = ClExpr::Commitment {
            debtor: "a".into(),
            creditor: "b".into(),
            act: ClAction {
                name: "acl.std.fs.file_read".into(),
                params: serde_json::json!({"path": "/x"}),
                result_schema: None,
                partial: None,
            },
            deadline_ms: None,
            status: crate::cl::CommitmentStatus::Proposed,
        };
        assert!(s.validate(&c).is_ok());
        let bad = ClExpr::Commitment {
            debtor: "a".into(),
            creditor: "b".into(),
            act: ClAction {
                name: "acl.std.fs.file_read".into(),
                params: serde_json::json!({"path": 7}),
                result_schema: None,
                partial: None,
            },
            deadline_ms: None,
            status: crate::cl::CommitmentStatus::Proposed,
        };
        assert!(s.validate(&bad).is_err());
    }

    #[test]
    fn json_roundtrip() {
        let s = schema();
        let json = serde_json::to_vec(&s).unwrap();
        let s2 = RuntimeSchema::from_json(&json).unwrap();
        assert_eq!(s.namespace, s2.namespace);
        assert!(s2.validate(&ClExpr::fact("acl.std.fs.file_exists", serde_json::json!({"path": "/x"}))).is_ok());
    }
}
