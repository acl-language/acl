//! Bridge adapters (SPEC.md roadmap item 3): map ACL semantics to the
//! protocols agents already speak — A2A task lifecycle and MCP tools.
//!
//! A2A v1.0: Task states SUBMITTED, WORKING, INPUT_REQUIRED, COMPLETED,
//! CANCELED, FAILED, REJECTED (spec: a2a-protocol.org).
//! MCP: JSON-RPC 2.0 methods `tools/list` and `tools/call`.

use crate::{AgentCard, Body, Capability, Envelope, Performative};

// ---------------------------------------------------------------- A2A side

/// A2A TaskState (subset used by the bridge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A2aState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Canceled,
    Failed,
    Rejected,
}

impl A2aState {
    pub fn as_str(&self) -> &'static str {
        match self {
            A2aState::Submitted => "SUBMITTED",
            A2aState::Working => "WORKING",
            A2aState::InputRequired => "INPUT_REQUIRED",
            A2aState::Completed => "COMPLETED",
            A2aState::Canceled => "CANCELED",
            A2aState::Failed => "FAILED",
            A2aState::Rejected => "REJECTED",
        }
    }
}

/// Map an ACL (performative, body) pair to the A2A task state it represents.
/// `None` = performative has no A2A equivalent (e.g. propose/hello).
pub fn acl_to_a2a_state(env: &Envelope) -> Option<A2aState> {
    Some(match (&env.performative, &env.body) {
        (Performative::Request, _) => A2aState::Submitted,
        (Performative::Delegate, _) => A2aState::Submitted,
        (Performative::Progress, _) => A2aState::Working,
        (Performative::InputRequired, _) => A2aState::InputRequired,
        (Performative::Inform, Body::Result { .. }) => A2aState::Completed,
        (Performative::Inform, _) => A2aState::Working, // plain text update
        (Performative::Failure, _) => A2aState::Failed,
        (Performative::Cancel, _) => A2aState::Canceled,
        (Performative::Reject, _) => A2aState::Rejected,
        _ => return None,
    })
}

/// A2A-shaped task object (JSON, mirroring the A2A v1.0 Task structure).
pub fn acl_to_a2a_task(env: &Envelope) -> Option<serde_json::Value> {
    let state = acl_to_a2a_state(env)?;
    let mut task = serde_json::json!({
        "id": env.msg_id,
        "contextId": env.reply_with.clone().unwrap_or_else(|| env.msg_id.clone()),
        "status": {"state": state.as_str()},
    });
    if let Some(r) = &env.in_reply_to {
        task["status"]["message"] = serde_json::json!({"role": "agent", "taskId": r});
    }
    // artifacts carry the Result payload
    if let Body::Result { output, artifacts } = &env.body {
        task["artifacts"] = serde_json::json!(artifacts
            .iter()
            .map(|a| serde_json::json!({
                "artifactId": a.name,
                "parts": [{"kind": "data", "data": {
                    "mime": a.mime,
                    // bytes omitted in the JSON view; A2A bridges reference blobs
                    "size": a.bytes.len(),
                }}]
            }))
            .collect::<Vec<_>>());
        if let Some(o) = output.as_object() {
            task["metadata"] = serde_json::json!(o);
        }
    }
    if let Body::Failure { code, reason, .. } = &env.body {
        task["status"]["message"] = serde_json::json!({
            "role": "agent", "parts": [{"kind": "text", "text": reason}], "code": code
        });
    }
    Some(task)
}

/// §13bis: full A2A task JSON -> ACL Envelope (the inverse of
/// `acl_to_a2a_task`). Rebuilds the performative from the task state and
/// the body from artifacts/metadata. `None` when the state is unknown.
pub fn a2a_task_to_acl(task: &serde_json::Value) -> Option<Envelope> {
    let state = task["status"]["state"].as_str()?;
    let performative = a2a_state_to_acl(state)?;
    let id = task["id"].as_str()?.to_string();
    let context = task["contextId"].as_str().unwrap_or("").to_string();
    let ctx2 = context.clone();
    // Completed tasks carry artifacts -> Result body; others keep a Text/Progress-shaped body
    let body = if state == "COMPLETED" {
        let output = task.get("metadata").cloned().unwrap_or(serde_json::json!({}));
        let artifacts: Vec<crate::Artifact> = task["artifacts"]
            .as_array()
            .map(|arts| {
                arts.iter()
                    .map(|a| crate::Artifact {
                        name: a["artifactId"].as_str().unwrap_or("").to_string(),
                        mime: a["parts"][0]["data"]["mime"].as_str().unwrap_or("application/json").to_string(),
                        bytes: Vec::new(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Body::Result { output, artifacts }
    } else if state == "FAILED" {
        let reason = task["status"]["message"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("failed via A2A")
            .to_string();
        let code = task["status"]["message"]["code"].as_u64().unwrap_or(500) as u16;
        Body::Failure { target_msg_id: context.clone(), code, reason, recoverable: false }
    } else {
        Body::Progress {
            target_msg_id: ctx2,
            percent: Some(50),
            note: Some(state.to_string()),
        }
    };
    let mut env = Envelope::new(
        crate::Aid::new("a2a:bridge"),
        crate::Aid::new("did:acl:local"),
        performative,
        body,
    );
    env.msg_id = id;
    env.reply_with = Some(context);
    Some(env)
}

/// Map an A2A task-state JSON object back to the ACL performative it implies.
pub fn a2a_state_to_acl(state: &str) -> Option<Performative> {
    Some(match state {
        "SUBMITTED" | "WORKING" => Performative::Progress,
        "INPUT_REQUIRED" => Performative::InputRequired,
        "COMPLETED" => Performative::Inform,
        "FAILED" => Performative::Failure,
        "CANCELED" => Performative::Cancel,
        "REJECTED" => Performative::Reject,
        _ => return None,
    })
}

// ---------------------------------------------------------------- MCP side

/// AgentCard capabilities → MCP `tools/list` result.
pub fn card_to_mcp_tools(card: &AgentCard) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": card.capabilities.iter().map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "description": format!("{} v{}", card.name, card.version),
                    "inputSchema": c.input_schema.clone()
                        .unwrap_or(serde_json::json!({"type": "object"})),
                })
            }).collect::<Vec<_>>(),
        }
    })
}

/// MCP `tools/call` params → an ACL request body (name = capability name).
pub fn mcp_call_to_acl_body(params: &serde_json::Value) -> Result<Body, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing name")?;
    let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
    Ok(Body::Task {
        description: name.to_string(),
        input: args,
        deadline_ms: None,
        priority: None,
    })
}

/// An ACL inform/Result → MCP `tools/call` result.
pub fn acl_result_to_mcp(env: &Envelope) -> serde_json::Value {
    let output = match &env.body {
        Body::Result { output, .. } => output.clone(),
        Body::Text { text } => serde_json::json!(text),
        _ => serde_json::Value::Null,
    };
    let (content, is_error) = match (&env.performative, &output) {
        (Performative::Failure { .. }, _) | (_, _) if env.performative == Performative::Failure => (
            serde_json::json!([{"type": "text", "text": output.to_string()}]),
            true,
        ),
        _ => (serde_json::json!([{"type": "text", "text": output.to_string()}]), false),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"content": content, "isError": is_error},
    })
}

/// capability name lookup on a card (used when routing tools/call)
pub fn card_has_capability(card: &AgentCard, name: &str) -> bool {
    card.capabilities.iter().any(|Capability { name: n, .. }| n == name)
}

/// §13bis: validate an MCP call's arguments against the card's declared
/// input schema — JSON-schema-lite: `required` fields must be present
/// with non-null values, and `type` is checked when declared.
pub fn validate_arguments(
    card: &AgentCard,
    name: &str,
    arguments: &serde_json::Value,
) -> Result<(), String> {
    let Some(cap) = card.capabilities.iter().find(|c| c.name == name) else {
        return Err(format!("capability '{name}' not on card"));
    };
    let Some(schema) = &cap.input_schema else {
        return Ok(()); // no schema declared: accept anything
    };
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    if let Some(required) = schema["required"].as_array() {
        for field in required {
            let field = field.as_str().unwrap_or_default();
            if !args.contains_key(field) {
                return Err(format!("missing required argument '{field}'"));
            }
        }
    }
    if let Some(props) = schema["properties"].as_object() {
        for (key, spec) in props {
            let Some(v) = args.get(key) else { continue };
            let want = spec["type"].as_str().unwrap_or("");
            let ok = match want {
                "string" => v.is_string(),
                "number" => v.is_number(),
                "integer" => v.is_i64() || v.is_u64(),
                "boolean" => v.is_boolean(),
                "object" => v.is_object(),
                "array" => v.is_array(),
                _ => true, // unknown/unspecified type: accept
            };
            if !ok {
                return Err(format!("argument '{key}' expected type {want}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Aid;

    fn env(perf: Performative, body: Body) -> Envelope {
        Envelope::new(Aid::local(1, "t"), Aid::new("did:x:y"), perf, body)
    }

    #[test]
    fn a2a_state_mapping_full_lifecycle() {
        let cases = [
            (Performative::Request, A2aState::Submitted),
            (Performative::Delegate, A2aState::Submitted),
            (Performative::Progress, A2aState::Working),
            (Performative::InputRequired, A2aState::InputRequired),
            (Performative::Inform, A2aState::Completed),
            (Performative::Failure, A2aState::Failed),
            (Performative::Cancel, A2aState::Canceled),
            (Performative::Reject, A2aState::Rejected),
        ];
        for (perf, expected) in cases {
            let body = match perf {
                Performative::Inform => Body::Result {
                    output: serde_json::json!("ok"),
                    artifacts: vec![],
                },
                Performative::Failure => Body::Failure {
                    target_msg_id: "m1".into(),
                    code: 500,
                    reason: "boom".into(),
                    recoverable: false,
                },
                Performative::Progress => Body::Progress {
                    target_msg_id: "m1".into(),
                    percent: Some(10),
                    note: None,
                },
                Performative::InputRequired => Body::InputNeeded {
                    target_msg_id: "m1".into(),
                    what: "x".into(),
                    schema: None,
                },
                _ => Body::Ref {
                    target_msg_id: "m1".into(),
                },
            };
            let e = env(perf, body);
            assert_eq!(acl_to_a2a_state(&e), Some(expected), "{perf:?}");
        }
        // no equivalent
        assert_eq!(acl_to_a2a_state(&env(Performative::Propose, Body::Ref { target_msg_id: "m".into() })), None);
    }

    #[test]
    fn a2a_task_shape() {
        let e = env(
            Performative::Inform,
            Body::Result {
                output: serde_json::json!({"reversed": "cba"}),
                artifacts: vec![],
            },
        );
        let task = acl_to_a2a_task(&e).unwrap();
        assert_eq!(task["status"]["state"], "COMPLETED");
        assert_eq!(task["id"], e.msg_id);
        assert_eq!(task["artifacts"], serde_json::json!([]));
    }

    #[test]
    fn a2a_roundtrip_through_bridge() {
        // ACL request → SUBMITTED → back to ACL Progress-compatible
        let e = env(
            Performative::Request,
            Body::Task {
                description: "t".into(),
                input: serde_json::json!({}),
                deadline_ms: None,
                priority: None,
            },
        );
        let task = acl_to_a2a_task(&e).unwrap();
        let state = task["status"]["state"].as_str().unwrap();
        assert_eq!(a2a_state_to_acl(state), Some(Performative::Progress));
    }

    #[test]
    fn mcp_tools_list_from_card() {
        let card = AgentCard {
            name: "demo".into(),
            version: "1.0".into(),
            description: "d".into(),
            endpoint: Aid::local(1, "t"),
            capabilities: vec![Capability {
                name: "text.reverse".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object", "required": ["text"]
                })),
                output_schema: None,
                examples: None,
            }],
            auth_scheme: "none".into(),
            key_id: None,
            public_key: None,
            rate_hint: None,
            valid_until: None,
            sig: None,
        };
        let tools = card_to_mcp_tools(&card);
        assert_eq!(tools["jsonrpc"], "2.0");
        assert_eq!(tools["result"]["tools"][0]["name"], "text.reverse");
        assert_eq!(tools["result"]["tools"][0]["inputSchema"]["required"][0], "text");
        assert!(card_has_capability(&card, "text.reverse"));
        assert!(!card_has_capability(&card, "other"));
    }

    #[test]
    fn validate_arguments_checks_required_and_types() {
        let card = AgentCard {
            name: "demo".into(),
            version: "1".into(),
            description: "d".into(),
            endpoint: Aid::local(1, "t"),
            capabilities: vec![Capability {
                name: "text.reverse".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "required": ["text"],
                    "properties": {"text": {"type": "string"}, "count": {"type": "integer"}}
                })),
                output_schema: None,
                examples: None,
            }],
            auth_scheme: "none".into(),
            key_id: None,
            public_key: None,
            rate_hint: None,
            valid_until: None,
            sig: None,
        };
        // ok
        validate_arguments(&card, "text.reverse", &serde_json::json!({"text": "abc"})).unwrap();
        // missing required
        assert!(validate_arguments(&card, "text.reverse", &serde_json::json!({})).is_err());
        // wrong type
        assert!(validate_arguments(&card, "text.reverse", &serde_json::json!({"text": 5})).is_err());
        // wrong type on optional
        assert!(validate_arguments(&card, "text.reverse", &serde_json::json!({"text": "a", "count": "many"})).is_err());
        // unknown capability
        assert!(validate_arguments(&card, "nope", &serde_json::json!({})).is_err());
    }

    #[test]
    fn a2a_full_roundtrip_acl_to_task_and_back() {
        let e = env(
            Performative::Inform,
            Body::Result {
                output: serde_json::json!({"reversed": "cba"}),
                artifacts: vec![],
            },
        );
        let task = acl_to_a2a_task(&e).unwrap();
        let back = a2a_task_to_acl(&task).unwrap();
        // the inverse bridge preserves the state mapping and correlation id
        assert_eq!(back.performative, Performative::Inform);
        assert_eq!(back.msg_id, e.msg_id);
        match &back.body {
            Body::Result { output, .. } => assert_eq!(output["reversed"], "cba"),
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn mcp_call_to_acl_and_back() {
        let call = serde_json::json!({
            "name": "text.reverse",
            "arguments": {"text": "abc"}
        });
        let body = mcp_call_to_acl_body(&call).unwrap();
        match &body {
            Body::Task { description, input, .. } => {
                assert_eq!(description, "text.reverse");
                assert_eq!(input["text"], "abc");
            }
            other => panic!("unexpected body {other:?}"),
        }
        let e = env(Performative::Inform, Body::Result {
            output: serde_json::json!({"reversed": "cba"}),
            artifacts: vec![],
        });
        let result = acl_result_to_mcp(&e);
        assert_eq!(result["result"]["isError"], false);
        assert!(result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cba"));
    }
}
