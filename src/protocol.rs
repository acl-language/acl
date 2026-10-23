//! `.aclp` — ACL protocol programs (spec §33).
//!
//! A protocol program turns the conversation itself into source code:
//! roles, session state, and handlers keyed on speech acts. The compile
//! step enforces the dialogue matrix (§16) at BUILD time — a program that
//! answers `query` with `request` does not compile.

use crate::dialogue::dialogue_legal;
use crate::idl::IdlError;
use crate::Performative;
use std::collections::BTreeMap;

// ---------------------------------------------------------------- model

#[derive(Debug, Clone, PartialEq)]
pub struct StateDecl {
    pub name: String,
    pub ty: Option<String>,
}

/// What fires a handler.
#[derive(Debug, Clone, PartialEq)]
pub enum OnTrigger {
    /// `on request("quote") from buyer` — incoming speech act (optionally
    /// patterned, optionally attributed to a sender role)
    Event { performative: Performative, pattern: Option<String>, from: Option<String> },
    /// `on breach` — a commitment this role owes was broken
    Breach,
    /// `on expired` — the session/commitment deadline passed
    Expired,
}

/// What a handler does. Phase 1: the speech-act statement set.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolAction {
    /// `send propose to seller` — MUST be dialogue-legal for the trigger;
    /// `to` names the receiving role (defaults to the counterpart)
    Send { performative: Performative, to: Option<String> },
    /// `begin` — activate the pending commitment (proposed -> active)
    Begin,
    /// `fulfil` — mark the commitment fulfilled (active -> fulfilled)
    Fulfil,
    /// `retract(fact_name)` — belief revision as a statement (§32.1)
    Retract { target: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub trigger: OnTrigger,
    pub actions: Vec<ProtocolAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndWhen {
    /// builtin: the commitment reached a terminal state
    Builtin(String), // fulfilled | expired | breached
    /// a declared session state is true
    State(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProtocolProgram {
    pub name: String,
    pub version: u32,
    pub roles: Vec<String>,
    pub states: Vec<StateDecl>,
    pub handlers: Vec<Handler>,
    pub end_when: Option<EndWhen>,
}

impl ProtocolProgram {
    pub fn state_names(&self) -> Vec<&str> {
        self.states.iter().map(|s| s.name.as_str()).collect()
    }
}

// ---------------------------------------------------------------- parse

fn perf_from_word(w: &str) -> Option<Performative> {
    Some(match w {
        "request" => Performative::Request,
        "inform" => Performative::Inform,
        "query" => Performative::Query,
        "propose" => Performative::Propose,
        "accept" => Performative::Accept,
        "reject" => Performative::Reject,
        "cancel" => Performative::Cancel,
        "progress" => Performative::Progress,
        "input_required" => Performative::InputRequired,
        "failure" => Performative::Failure,
        "delegate" => Performative::Delegate,
        "error" => Performative::Error,
        "hello" => Performative::Hello,
        "bye" => Performative::Bye,
        "retract" => Performative::Retract,
        "subscribe" => Performative::Subscribe,
        "unsubscribe" => Performative::Unsubscribe,
        _ => return None,
    })
}

/// Strip `(...)` pattern and `from <role>` from a handler head:
/// `request("quote") from buyer` -> (`request`, Some(`"quote"`), Some(`buyer`))
fn split_trigger(head: &str) -> (String, Option<String>, Option<String>) {
    let (head, from) = match head.rfind(" from ") {
        Some(i) => (head[..i].trim().to_string(), Some(head[i + 6..].trim().to_string())),
        None => (head.trim().to_string(), None),
    };
    let (word, pattern) = match head.find('(') {
        Some(i) if head.ends_with(')') => {
            (head[..i].trim().to_string(), Some(head[i..].to_string()))
        }
        _ => (head.trim().to_string(), None),
    };
    (word, pattern, from)
}

pub fn parse(src: &str) -> Result<ProtocolProgram, Vec<IdlError>> {
    let mut errors = Vec::new();
    let mut prog = ProtocolProgram::default();
    let mut seen_protocol = false;
    let mut handler: Option<Handler> = None;

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        if raw.trim_start().starts_with("//") {
            continue;
        }
        let line = match raw.find("//") {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }

        // inside a handler block: collect actions until `}`
        if let Some(h) = handler.as_mut() {
            if line == "}" {
                if h.actions.is_empty() {
                    errors.push(IdlError { line: line_no, message: "empty handler block".into() });
                }
                prog.handlers.push(handler.take().unwrap());
                continue;
            }
            let action = match parse_action(line) {
                Ok(a) => a,
                Err(e) => {
                    errors.push(IdlError { line: line_no, message: e });
                    continue;
                }
            };
            h.actions.push(action);
            continue;
        }

        if let Some(rest) = line.strip_prefix("protocol ") {
            let body = rest.trim_end_matches(';').trim();
            let mut parts = body.split_whitespace();
            let name = parts.next().unwrap_or_default().to_string();
            let ver = parts
                .next()
                .and_then(|v| v.strip_prefix('v'))
                .and_then(|v| v.parse().ok());
            if name.is_empty() || ver.is_none() || parts.next().is_some() {
                errors.push(IdlError {
                    line: line_no,
                    message: format!("bad protocol decl: '{line}' (want 'protocol name v1;')"),
                });
                continue;
            }
            if seen_protocol {
                errors.push(IdlError { line: line_no, message: "duplicate protocol declaration".into() });
                continue;
            }
            seen_protocol = true;
            prog.name = name;
            prog.version = ver.unwrap();
            continue;
        }
        if let Some(rest) = line.strip_prefix("roles ") {
            let list = rest.trim_end_matches(';');
            for r in list.split(',') {
                let r = r.trim();
                if r.is_empty() {
                    errors.push(IdlError { line: line_no, message: "empty role name".into() });
                } else {
                    prog.roles.push(r.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("state ") {
            let body = rest.trim_end_matches(';').trim();
            match body.split_once(':') {
                Some((name, ty)) => prog.states.push(StateDecl {
                    name: name.trim().to_string(),
                    ty: Some(ty.trim().to_string()),
                }),
                None => prog.states.push(StateDecl { name: body.to_string(), ty: None }),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("on ") {
            let (head, arrow, sends) = match rest.split_once("->") {
                Some((h, s)) => (h.trim(), true, s.trim().to_string()),
                None => (rest.trim_end_matches('{').trim(), false, String::new()),
            };
            let trigger = match parse_trigger(&head) {
                Ok(t) => t,
                Err(e) => {
                    errors.push(IdlError { line: line_no, message: e });
                    continue;
                }
            };
            let mut h = Handler { trigger, actions: Vec::new() };
            if arrow {
                // one-line form: `on X -> send Y;` (no block)
                let stmt = sends.trim_end_matches(';').trim();
                match parse_action(stmt) {
                    Ok(a) => h.actions.push(a),
                    Err(e) => errors.push(IdlError { line: line_no, message: e }),
                }
                prog.handlers.push(h);
            } else if line.ends_with('{') {
                handler = Some(h);
            } else {
                errors.push(IdlError { line: line_no, message: format!("malformed handler: '{line}' (want 'on <event> -> <action>;' or 'on <event> {{')") });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("end when ") {
            let cond = rest.trim_end_matches(';').trim();
            prog.end_when = Some(match cond {
                "fulfilled" | "expired" | "breached" => EndWhen::Builtin(cond.into()),
                s => EndWhen::State(s.into()),
            });
            continue;
        }
        if line == "end;" {
            continue; // optional terminator
        }
        errors.push(IdlError { line: line_no, message: format!("unrecognized line: '{line}'") });
    }

    if handler.is_some() {
        errors.push(IdlError { line: 0, message: "unterminated handler block (missing '}')".into() });
    }
    if !seen_protocol {
        errors.push(IdlError { line: 0, message: "missing 'protocol <name> v1;' declaration".into() });
    }
    if errors.is_empty() { Ok(prog) } else { Err(errors) }
}

fn parse_trigger(head: &str) -> Result<OnTrigger, String> {
    let (word, pattern, from) = split_trigger(head);
    match word.as_str() {
        "breach" | "expired" if from.is_some() => {
            Err(format!("'{word}' cannot have a 'from' role"))
        }
        "breach" => Ok(OnTrigger::Breach),
        "expired" => Ok(OnTrigger::Expired),
        w => match perf_from_word(w) {
            Some(p) => Ok(OnTrigger::Event { performative: p, pattern, from }),
            None => Err(format!("unknown trigger '{word}'")),
        },
    }
}

fn parse_action(stmt: &str) -> Result<ProtocolAction, String> {
    let s = stmt.trim().trim_end_matches(';').trim();
    if s == "begin" {
        return Ok(ProtocolAction::Begin);
    }
    if s == "fulfil" {
        return Ok(ProtocolAction::Fulfil);
    }
    if let Some(rest) = s.strip_prefix("retract(") {
        let target = rest.trim_end_matches(')').trim();
        if target.is_empty() {
            return Err("retract needs a target".into());
        }
        return Ok(ProtocolAction::Retract { target: target.trim_matches('"').to_string() });
    }
    if let Some(rest) = s.strip_prefix("send ") {
        let (rest, to) = match rest.find(" to ") {
            Some(i) => (rest[..i].trim(), Some(rest[i + 4..].trim().to_string())),
            None => (rest, None),
        };
        let word = rest.split(|c: char| c == '(' || c == ' ' || c == '{').next().unwrap_or(rest);
        return match perf_from_word(word.trim()) {
            Some(p) => Ok(ProtocolAction::Send { performative: p, to }),
            None => Err(format!("unknown performative in send: '{word}'")),
        };
    }
    Err(format!("unknown statement: '{s}'"))
}

// ---------------------------------------------------------------- check

/// Compile-time well-formedness — the dialogue matrix (§16) applied to the
/// PROGRAM, not the runtime. Returns warnings; errors abort compilation.
pub fn check(prog: &ProtocolProgram) -> Result<Vec<String>, Vec<IdlError>> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if prog.roles.is_empty() {
        errors.push(IdlError { line: 0, message: "protocol declares no roles".into() });
    } else {
        let mut seen = std::collections::BTreeSet::new();
        for r in &prog.roles {
            if !seen.insert(r.clone()) {
                errors.push(IdlError { line: 0, message: format!("duplicate role '{r}'") });
            }
        }
    }
    if prog.handlers.is_empty() {
        errors.push(IdlError { line: 0, message: "protocol declares no handlers".into() });
    }

    // duplicate (trigger, pattern) handlers are ambiguous
    let mut sigs: BTreeMap<String, ()> = BTreeMap::new();
    for h in &prog.handlers {
        let sig = match &h.trigger {
            OnTrigger::Event { performative, pattern, from } => {
                format!("event:{performative:?}:{pattern:?}:{from:?}")
            }
            OnTrigger::Breach => "breach".into(),
            OnTrigger::Expired => "expired".into(),
        };
        if sigs.insert(sig.clone(), ()).is_some() {
            errors.push(IdlError { line: 0, message: format!("duplicate handler for {sig}") });
        }
    }

    // §33: every send must be dialogue-legal for its trigger
    for h in &prog.handlers {
        for a in &h.actions {
            let ProtocolAction::Send { performative: out, .. } = a else { continue };
            let legal = match &h.trigger {
                OnTrigger::Event { performative: inp, .. } => dialogue_legal(*inp, *out),
                // breach/expired happen inside a session opened by some
                // request-shaped move; the send must be legal after AT
                // LEAST ONE plausible predecessor
                OnTrigger::Breach | OnTrigger::Expired => [
                    Performative::Request,
                    Performative::Propose,
                    Performative::Delegate,
                ]
                .iter()
                .any(|prev| dialogue_legal(*prev, *out)),
            };
            if !legal {
                let trig = match &h.trigger {
                    OnTrigger::Event { performative, .. } => format!("{performative:?}"),
                    OnTrigger::Breach => "Breach".into(),
                    OnTrigger::Expired => "Expired".into(),
                };
                errors.push(IdlError {
                    line: 0,
                    message: format!(
                        "dialogue violation: handler on {trig} cannot send {out:?} (§16 matrix)"
                    ),
                });
            }
        }
    }

    // end_when must reference a declared state or a builtin
    if let Some(ew) = &prog.end_when {
        if let EndWhen::State(s) = ew {
            if !prog.state_names().contains(&s.as_str()) {
                errors.push(IdlError {
                    line: 0,
                    message: format!("end when references undeclared state '{s}'"),
                });
            }
        }
    }

    // begin/fulfil without any Event handler is meaningless
    let has_event = prog
        .handlers
        .iter()
        .any(|h| matches!(h.trigger, OnTrigger::Event { .. }));
    if !has_event {
        warnings.push("protocol has no event handlers (nothing can arrive)".into());
    }

    if errors.is_empty() { Ok(warnings) } else { Err(errors) }
}

// ---------------------------------------------------------------- pretty

pub fn pretty(prog: &ProtocolProgram) -> String {
    let mut out = String::new();
    out.push_str(&format!("protocol {} v{};\n", prog.name, prog.version));
    out.push_str(&format!("roles {};\n", prog.roles.join(", ")));
    for s in &prog.states {
        match &s.ty {
            Some(t) => out.push_str(&format!("state {}: {};\n", s.name, t)),
            None => out.push_str(&format!("state {};\n", s.name)),
        }
    }
    for h in &prog.handlers {
        let trig = match &h.trigger {
            OnTrigger::Event { performative, pattern, from } => {
                let mut t = match pattern {
                    Some(p) => format!("{}{}", perf_word(performative), p),
                    None => perf_word(performative).to_string(),
                };
                if let Some(f) = from {
                    t.push_str(&format!(" from {f}"));
                }
                t
            }
            OnTrigger::Breach => "breach".into(),
            OnTrigger::Expired => "expired".into(),
        };
        if h.actions.len() == 1 {
            out.push_str(&format!("on {} -> {};\n", trig, action_word(&h.actions[0])));
        } else {
            out.push_str(&format!("on {} {{\n", trig));
            for a in &h.actions {
                out.push_str(&format!("  {};\n", action_word(a)));
            }
            out.push_str("}\n");
        }
    }
    if let Some(ew) = &prog.end_when {
        match ew {
            EndWhen::Builtin(b) => out.push_str(&format!("end when {b};\n")),
            EndWhen::State(s) => out.push_str(&format!("end when {s};\n")),
        }
    }
    out
}

fn perf_word(p: &Performative) -> &'static str {
    match p {
        Performative::Request => "request",
        Performative::Inform => "inform",
        Performative::Query => "query",
        Performative::Propose => "propose",
        Performative::Accept => "accept",
        Performative::Reject => "reject",
        Performative::Cancel => "cancel",
        Performative::Progress => "progress",
        Performative::InputRequired => "input_required",
        Performative::Failure => "failure",
        Performative::Delegate => "delegate",
        Performative::Error => "error",
        Performative::Hello => "hello",
        Performative::Bye => "bye",
        Performative::Retract => "retract",
        Performative::Subscribe => "subscribe",
        Performative::Unsubscribe => "unsubscribe",
    }
}

fn action_word(a: &ProtocolAction) -> String {
    match a {
        ProtocolAction::Send { performative, to } => match to {
            Some(r) => format!("send {} to {}", perf_word(performative), r),
            None => format!("send {}", perf_word(performative)),
        },
        ProtocolAction::Begin => "begin".into(),
        ProtocolAction::Fulfil => "fulfil".into(),
        ProtocolAction::Retract { target } => format!("retract({target})"),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
// supply-check protocol program
protocol supply_check v1;
roles buyer, seller;
state quoted: bool;
state paid: bool;

// seller side: a request for a quote gets a proposal
on request("quote") -> send propose;
on accept -> begin;
on query("status") -> send inform;
on breach -> send failure;
on expired -> send error;

end when fulfilled;
"#;

    #[test]
    fn parses_and_checks_good_program() {
        let p = parse(GOOD).expect("parse");
        assert_eq!(p.name, "supply_check");
        assert_eq!(p.version, 1);
        assert_eq!(p.roles, vec!["buyer", "seller"]);
        assert_eq!(p.states.len(), 2);
        assert_eq!(p.handlers.len(), 5);
        // §16 at compile time: every send is dialogue-legal for its trigger
        check(&p).expect("check");
        // pretty roundtrip
        let reparsed = parse(&pretty(&p)).expect("reparse pretty");
        assert_eq!(reparsed, p);
    }

    #[test]
    fn dialogue_violation_does_not_compile() {
        // answering a query with a request is illegal per §16
        let src = r#"
protocol bad v1;
roles a, b;
on query -> send request;
"#;
        let p = parse(src).expect("parse");
        let errs = check(&p).expect_err("must fail");
        assert!(errs[0].message.contains("dialogue violation"));
        assert!(errs[0].message.contains("§16"));
    }

    #[test]
    fn breach_handlers_constrained_by_plausible_predecessors() {
        // failure after breach is fine (legal after request)
        let ok = parse("protocol p v1;\nroles a,b;\non breach -> send failure;\n").unwrap();
        check(&ok).expect("failure legal after breach");
        // hello after breach is NOT legal after request/propose/delegate
        let bad = parse("protocol p v1;\nroles a,b;\non breach -> send hello;\n").unwrap();
        assert!(check(&bad).is_err());
    }

    #[test]
    fn duplicate_handlers_rejected() {
        let src = r#"
protocol p v1;
roles a, b;
on query -> send inform;
on query -> send inform;
"#;
        let p = parse(src).unwrap();
        let errs = check(&p).expect_err("duplicate");
        assert!(errs.iter().any(|e| e.message.contains("duplicate handler")));
    }

    #[test]
    fn end_when_requires_declared_state() {
        let src = r#"
protocol p v1;
roles a, b;
on query -> send inform;
end when nowhere;
"#;
        let p = parse(src).unwrap();
        let errs = check(&p).expect_err("bad end_when");
        assert!(errs.iter().any(|e| e.message.contains("undeclared state 'nowhere'")));
    }

    #[test]
    fn multi_action_handler_and_retract_statement() {
        let src = r#"
protocol p v1;
roles a, b;
state announced: bool;
on request("job") {
  send propose;
  retract(announced);
}
"#;
        let p = parse(src).unwrap();
        assert_eq!(p.handlers[0].actions.len(), 2);
        assert_eq!(
            p.handlers[0].actions[1],
            ProtocolAction::Retract { target: "announced".into() }
        );
        check(&p).expect("check");
    }

    #[test]
    fn unterminated_block_and_missing_protocol() {
        assert!(parse("on query -> send inform;").is_err());
        assert!(parse("protocol p v1;\nroles a;\non query {").is_err());
        let e = parse("roles a;").unwrap_err();
        assert!(e.iter().any(|x| x.message.contains("missing 'protocol")));
    }
}
