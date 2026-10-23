//! `.aclp` — ACL protocol programs (spec §33).
//!
//! A protocol program turns the conversation itself into source code:
//! roles, session state, and handlers keyed on speech acts. The compile
//! step enforces the dialogue matrix (§16) at BUILD time — a program that
//! answers `query` with `request` does not compile.

use crate::dialogue::dialogue_legal;
use crate::runtime::handler_executor;
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
    /// §34: `every 60s while <guard> as <role>` — time-driven handler;
    /// the HOST injects ticks (runtime stays clock-free and deterministic).
    /// Unattributed timers fire in every role.
    Timer { every_ms: i64, guard: Option<Guard>, as_role: Option<String> },
}

/// What a handler does. Phase 1: the speech-act statement set.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolAction {
    /// `send propose to seller` — MUST be dialogue-legal for the trigger;
    /// `to` names the receiving role (defaults to the counterpart)
    Send { performative: Performative, to: Option<String>, fields: Option<BTreeMap<String, String>> },
    /// `begin` — activate the pending commitment (proposed -> active)
    Begin,
    /// `fulfil` — mark the commitment fulfilled (active -> fulfilled)
    Fulfil,
    /// `retract(fact_name)` — belief revision as a statement (§32.1)
    Retract { target: String },
}

/// §34: guard — boolean expression over session states, evaluated at
/// dispatch time. `quoted`, `not quoted`, `a and b`, `a or b`.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    Atom { name: String, neg: bool },
    And(Box<Guard>, Box<Guard>),
    Or(Box<Guard>, Box<Guard>),
}

impl Guard {
    pub fn eval(&self, states: &BTreeMap<String, bool>) -> bool {
        match self {
            Guard::Atom { name, neg } => {
                let v = states.get(name).copied().unwrap_or(false);
                if *neg { !v } else { v }
            }
            Guard::And(a, b) => a.eval(states) && b.eval(states),
            Guard::Or(a, b) => a.eval(states) || b.eval(states),
        }
    }
    pub fn names(&self) -> Vec<&str> {
        match self {
            Guard::Atom { name, .. } => vec![name.as_str()],
            Guard::And(a, b) | Guard::Or(a, b) => {
                let mut v = a.names();
                v.extend(b.names());
                v
            }
        }
    }
}

/// parse `a and not b or c` — precedence: not > and > or (left-assoc)
pub fn parse_guard(src: &str) -> Result<Guard, String> {
    fn or(it: &mut std::iter::Peekable<std::str::SplitAsciiWhitespace<'_>>) -> Result<Guard, String> {
        let mut left = and(it)?;
        while it.peek() == Some(&"or") {
            it.next();
            let right = and(it)?;
            left = Guard::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn and(it: &mut std::iter::Peekable<std::str::SplitAsciiWhitespace<'_>>) -> Result<Guard, String> {
        let mut left = atom(it)?;
        while it.peek() == Some(&"and") {
            it.next();
            let right = atom(it)?;
            left = Guard::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn atom(it: &mut std::iter::Peekable<std::str::SplitAsciiWhitespace<'_>>) -> Result<Guard, String> {
        let w = it.next().ok_or_else(|| "empty guard".to_string())?;
        if w == "not" {
            let n = it.next().ok_or_else(|| "'not' needs a state".to_string())?;
            return Ok(Guard::Atom { name: n.to_string(), neg: true });
        }
        Ok(Guard::Atom { name: w.to_string(), neg: false })
    }
    let mut it = src.split_ascii_whitespace().peekable();
    let g = or(&mut it)?;
    if it.peek().is_some() {
        return Err(format!("trailing tokens in guard: '{src}'"));
    }
    Ok(g)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub trigger: OnTrigger,
    pub guard: Option<Guard>,
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

impl Handler {
    /// §34: true when the trigger pattern is a content pattern `{"k": "v"}`
    /// (as opposed to a literal `("quote")` equality pattern)
    pub fn pattern_is_json(&self) -> bool {
        matches!(&self.trigger, OnTrigger::Event { pattern: Some(p), .. } if p.starts_with('{'))
    }
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
    let (word, pattern) = match (head.find('('), head.find('{')) {
        (Some(i), _) if head.ends_with(')') => {
            (head[..i].trim().to_string(), Some(head[i..].to_string()))
        }
        (_, Some(i)) if head.ends_with('}') => {
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
        if let Some(rest) = line.strip_prefix("every ") {
            // §34: time-driven handler — `every 60s while <guard> -> act;`
            let (spec, arrow, sends) = match rest.split_once("->") {
                Some((h, s)) => (h.trim(), true, s.trim().to_string()),
                None => (rest.trim_end_matches('{').trim(), false, String::new()),
            };
            // peel `as <role>` BEFORE the guard split (the guard would
            // otherwise swallow it: 'active as a')
            let (spec, as_role) = match spec.rfind(" as ") {
                Some(i) => (spec[..i].trim(), Some(spec[i + 4..].trim().to_string())),
                None => (spec, None),
            };
            let (spec, guard_src) = match spec.split_once(" while ") {
                Some((a, g)) => (a.trim(), Some(g.trim().to_string())),
                None => (spec, None),
            };
            let every_ms = parse_duration(spec);
            let Some(every_ms) = every_ms else {
                errors.push(IdlError { line: line_no, message: format!("bad timer spec: '{spec}' (want e.g. '60s', '500ms')") });
                continue;
            };
            let guard = match guard_src.map(|g| parse_guard(&g)) {
                Some(Ok(g)) => Some(g),
                Some(Err(e)) => {
                    errors.push(IdlError { line: line_no, message: format!("bad guard: {e}") });
                    continue;
                }
                None => None,
            };
            let h = Handler { trigger: OnTrigger::Timer { every_ms, guard, as_role }, guard: None, actions: Vec::new() };
            if arrow {
                let stmt = sends.trim_end_matches(';').trim();
                match parse_action(stmt) {
                    Ok(a) => {
                        let mut hh = h;
                        hh.actions.push(a);
                        prog.handlers.push(hh);
                    }
                    Err(e) => errors.push(IdlError { line: line_no, message: e }),
                }
            } else {
                handler = Some(h);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("on ") {
            let (head, arrow, sends) = match rest.split_once("->") {
                Some((h, s)) => (h.trim(), true, s.trim().to_string()),
                None => (rest.trim_end_matches('{').trim(), false, String::new()),
            };
            // §34: peel `when <guard>` (before from-parsing)
            let (head, guard) = match head.rfind(" when ") {
                Some(i) => (head[..i].trim(), match parse_guard(head[i + 6..].trim()) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        errors.push(IdlError { line: line_no, message: format!("bad guard: {e}") });
                        continue;
                    }
                }),
                None => (head, None),
            };
            let trigger = match parse_trigger(&head) {
                Ok(t) => t,
                Err(e) => {
                    errors.push(IdlError { line: line_no, message: e });
                    continue;
                }
            };
            let mut h = Handler { trigger, guard, actions: Vec::new() };
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

fn parse_duration(spec: &str) -> Option<i64> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    match unit {
        "ms" => Some(n),
        "s" => Some(n * 1000),
        "m" => Some(n * 60_000),
        _ => None,
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
        // optional payload: `send progress {"pct": "50"}`
        let (rest, fields) = match (rest.find('{'), rest.ends_with('}')) {
            (Some(i), true) => {
                let raw = &rest[i..];
                let map: BTreeMap<String, String> = serde_json::from_str(raw)
                    .map_err(|e| format!("bad send payload: {e}"))?;
                (rest[..i].trim(), Some(map))
            }
            _ => (rest, None),
        };
        let word = rest.split(|c: char| c == '(' || c == ' ' || c == '{').next().unwrap_or(rest);
        return match perf_from_word(word.trim()) {
            Some(p) => Ok(ProtocolAction::Send { performative: p, to, fields }),
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
                format!(
                    "event:{performative:?}:{pattern:?}:{from:?}:guard={:?}",
                    h.guard.as_ref().map(|g| guard_word(g))
                )
            }
            OnTrigger::Timer { every_ms, guard, as_role } => {
                format!("timer:{every_ms}:{guard:?}:{as_role:?}")
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
                // breach/expired/timer happen inside a session opened by some
                // request-shaped move; the send must be legal after AT
                // LEAST ONE plausible predecessor
                OnTrigger::Breach | OnTrigger::Expired | OnTrigger::Timer { .. } => [
                    Performative::Request,
                    Performative::Propose,
                    Performative::Delegate,
                ]
                .iter()
                .any(|prev| dialogue_legal(*prev, *out)),
            };
            if !legal {
                let mut trig = match &h.trigger {
                    OnTrigger::Event { performative, .. } => format!("{performative:?}"),
                    OnTrigger::Breach => "Breach".into(),
                    OnTrigger::Expired => "Expired".into(),
                    OnTrigger::Timer { .. } => "Timer".into(),
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

    // ---- §34: guards reference DECLARED BOOL states ----
    let state_ty: BTreeMap<&str, &str> = prog
        .states
        .iter()
        .map(|s| (s.name.as_str(), s.ty.as_deref().unwrap_or("bool")))
        .collect();
    let mut guard_ok = |g: &Guard, what: &str, errors: &mut Vec<IdlError>| {
        for n in g.names() {
            match state_ty.get(n) {
                None => errors.push(IdlError {
                    line: 0,
                    message: format!("{what} references undeclared state '{n}'"),
                }),
                Some(t) if *t != "bool" => errors.push(IdlError {
                    line: 0,
                    message: format!("{what} requires bool state '{n}', declared '{t}'"),
                }),
                _ => {}
            }
        }
    };
    for h in &prog.handlers {
        if let Some(g) = &h.guard {
            guard_ok(g, "handler guard", &mut errors);
        }
        if let OnTrigger::Timer { guard: Some(g), as_role, .. } = &h.trigger {
            guard_ok(g, "timer guard", &mut errors);
            if let Some(r) = as_role {
                if !prog.roles.iter().any(|x| x == r) {
                    errors.push(IdlError {
                        line: 0,
                        message: format!("timer 'as {r}': undeclared role"),
                    });
                }
            }
        }
    }
    if let Some(EndWhen::State(sname)) = &prog.end_when {
        if let Some(t) = state_ty.get(sname.as_str()) {
            if *t != "bool" {
                errors.push(IdlError {
                    line: 0,
                    message: format!("end when requires bool state '{sname}', declared '{t}'"),
                });
            }
        }
    }

    // ---- §34: n>2 protocols MUST attribute triggers and sends ----
    let n_roles = prog.roles.len();
    if n_roles > 2 {
        for h in &prog.handlers {
            if let OnTrigger::Event { from: None, .. } = &h.trigger {
                errors.push(IdlError {
                    line: 0,
                    message: format!(
                        "{n_roles}-role protocol: trigger 'on {}' needs 'from <role>'",
                        match &h.trigger {
                            OnTrigger::Event { performative, .. } => format!("{performative:?}"),
                            _ => unreachable!(),
                        }
                    ),
                });
            }
            for a in &h.actions {
                if let ProtocolAction::Send { performative, to: None, .. } = a {
                    if *performative == Performative::Bye {
                        continue; // session terminator
                    }
                    errors.push(IdlError {
                        line: 0,
                        message: format!(
                            "{n_roles}-role protocol: 'send {}' needs 'to <role>'",
                            format!("{performative:?}")
                        ),
                    });
                }
            }
        }
    }

    // ---- §34: LINEARITY — every send must have a receiver ----
    // receiver role = explicit `to` (else counterpart in 2-role); that
    // role's projection must contain a handler `on <perf>` (or breach/
    // expired/timer counts for state events). Orphan send = compile error.
    let counterpart = |role: &str| -> String {
        prog.roles
            .iter()
            .map(|s| s.as_str())
            .find(|r| *r != role)
            .unwrap_or("")
            .to_string()
    };
    for h in &prog.handlers {
        // executors of this handler emit its sends
        for exec in handler_executor(prog, h) {
            for a in &h.actions {
                let ProtocolAction::Send { performative, to, .. } = a else { continue };
                if *performative == Performative::Bye {
                    continue; // session terminator: needs no receiver
                }
                let target = to.clone().unwrap_or_else(|| counterpart(exec));
                let answered = prog.handlers.iter().any(|h2| {
                    // h2 runs in `target` and listens for this performative
                    handler_executor(prog, h2).contains(&target.as_str())
                        && match &h2.trigger {
                            OnTrigger::Event { performative: p2, .. } => p2 == performative,
                            _ => false,
                        }
                });
                if !answered {
                    errors.push(IdlError {
                        line: 0,
                        message: format!(
                            "unanswered send: '{exec}' sends {performative:?} to '{target}' but no handler 'on {performative:?}' exists there (§34 linearity)"
                        ),
                    });
                }
            }
        }
    }

    // ---- §34: LIVENESS — every handler can reach a terminal action ----
    // terminal: a handler containing `fulfil`, or one whose send is a
    // session-ender (bye/reject/error). Edges: handler -> handlers that
    // listen for its sends (in the target role). BFS from each handler.
    let is_terminal = |h: &Handler| {
        h.actions.iter().any(|a| matches!(a, ProtocolAction::Fulfil))
            || h.actions.iter().any(|a| matches!(
                a,
                ProtocolAction::Send { performative: Performative::Bye | Performative::Reject | Performative::Error, .. }
            ))
    };
    let edges = |from_h: usize| -> Vec<usize> {
        let mut out = Vec::new();
        for a in &prog.handlers[from_h].actions {
            let ProtocolAction::Send { performative, to, .. } = a else { continue };
            for (j, h2) in prog.handlers.iter().enumerate() {
                if j == from_h {
                    continue;
                }
                let listens = matches!(&h2.trigger, OnTrigger::Event { performative: p2, .. } if p2 == performative);
                if !listens {
                    continue;
                }
                // h2's executors must include the send's target role
                for exec in handler_executor(prog, h2) {
                    let target = to.clone().unwrap_or_else(|| counterpart(exec));
                    if handler_executor(prog, &prog.handlers[from_h]).contains(&target.as_str()) {
                        out.push(j);
                        break;
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    };
    for i in 0..prog.handlers.len() {
        let mut seen = std::collections::BTreeSet::new();
        let mut stack = vec![i];
        let mut reaches_terminal = is_terminal(&prog.handlers[i]);
        while let Some(n) = stack.pop() {
            for e in edges(n) {
                if seen.insert(e) {
                    if is_terminal(&prog.handlers[e]) {
                        reaches_terminal = true;
                    }
                    stack.push(e);
                }
            }
        }
        if !reaches_terminal {
            warnings.push(format!(
                "liveness: handler #{i} cannot reach a terminal action (fulfil/bye/reject/error) — possible deadlock"
            ));
        }
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
        let mut trig = match &h.trigger {
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
            OnTrigger::Timer { every_ms, guard, as_role } => {
                let mut t = if every_ms % 60_000 == 0 {
                    format!("every {}m", every_ms / 60_000)
                } else if every_ms % 1000 == 0 {
                    format!("every {}s", every_ms / 1000)
                } else {
                    format!("every {}ms", every_ms)
                };
                if let Some(g) = guard {
                    t.push_str(&format!(" while {}", guard_word(g)));
                }
                if let Some(r) = as_role {
                    t.push_str(&format!(" as {r}"));
                }
                t
            }
        };
        if let Some(g) = &h.guard {
            if !matches!(h.trigger, OnTrigger::Timer { .. }) {
                trig.push_str(&format!(" when {}", guard_word(g)));
            }
        }
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

fn guard_word(g: &Guard) -> String {
    match g {
        Guard::Atom { name, neg } => {
            if *neg { format!("not {name}") } else { name.clone() }
        }
        Guard::And(a, b) => format!("{} and {}", guard_word(a), guard_word(b)),
        Guard::Or(a, b) => format!("{} or {}", guard_word(a), guard_word(b)),
    }
}

fn action_word(a: &ProtocolAction) -> String {
    match a {
        ProtocolAction::Send { performative, to, fields } => {
            let mut out = match to {
                Some(r) => format!("send {} to {}", perf_word(performative), r),
                None => format!("send {}", perf_word(performative)),
            };
            if let Some(f) = fields {
                out.push_str(&format!(" {}", serde_json::to_string(f).unwrap_or_default()));
            }
            out
        }
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
// supply-check protocol program — fully linear (§34): every send has a receiver
protocol supply_check v1;
roles buyer, seller;
state quoted: bool;
state paid: bool;

// seller side
on request("quote") from buyer -> send propose to buyer;
on accept from buyer -> begin;
on query("status") from buyer -> send inform to buyer;
on breach -> send failure to buyer;
on expired -> send error to buyer;
// buyer side
on propose from seller -> send accept to seller;
on inform from seller -> send bye;
on failure from seller -> send bye;
on error from seller -> send bye;

end when fulfilled;
"#;

    #[test]
    fn parses_and_checks_good_program() {
        let p = parse(GOOD).expect("parse");
        assert_eq!(p.name, "supply_check");
        assert_eq!(p.version, 1);
        assert_eq!(p.roles, vec!["buyer", "seller"]);
        assert_eq!(p.states.len(), 2);
        assert_eq!(p.handlers.len(), 9); // 5 seller + 4 buyer
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
on request -> send bye;
"#;
        let p = parse(src).expect("parse");
        let errs = check(&p).expect_err("must fail");
        assert!(errs.iter().any(|e| e.message.contains("dialogue violation")));
        assert!(errs[0].message.contains("§16"));
    }

    #[test]
    fn breach_handlers_constrained_by_plausible_predecessors() {
        // failure after breach is fine (legal after request)
        let ok = parse("protocol p v1;\nroles a,b;\non breach -> send failure;\non failure -> send bye;\n").unwrap();
        check(&ok).expect("failure legal after breach");
        // hello after breach is NOT legal after request/propose/delegate
        let bad = parse("protocol p v1;\nroles a,b;\non breach -> send hello;\non hello -> send bye;\n").unwrap();
        assert!(check(&bad).is_err());
    }

    #[test]
    fn duplicate_handlers_rejected() {
        let src = r#"
protocol p v1;
roles a, b;
on query -> send inform;
on query -> send inform;
on inform -> send bye;
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
state done: bool;
on query -> send inform;
on inform -> begin;
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
on propose -> send bye;
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
    fn guards_parse_and_validate() {
        let src = r#"
protocol p v1;
roles a, b;
state quoted: bool;
state paid: bool;
on accept from b when quoted -> begin;
on accept from b when not quoted -> send error to b;
on request from b when quoted and not paid -> send propose to b;
on error from a -> send bye;
on propose from a -> send bye;
"#;
        let p = parse(src).expect("parse");
        // guard AST shape
        match &p.handlers[2].guard {
            Some(Guard::And(l, r)) => {
                assert!(matches!(**l, Guard::Atom { ref name, neg: false } if name == "quoted"));
                assert!(matches!(**r, Guard::Atom { ref name, neg: true } if name == "paid"));
            }
            other => panic!("expected And guard, got {other:?}"),
        }
        check(&p).expect("guards reference declared bool states");
    }

    #[test]
    fn guards_reject_undeclared_and_non_bool_states() {
        // undeclared state in guard
        let bad = parse("protocol p v1;\nroles a,b;\non accept from b when nowhere -> begin;\non error from b -> send bye;\n").unwrap();
        let errs = check(&bad).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("undeclared state 'nowhere'")));
        // non-bool state in guard
        let bad2 = parse("protocol p v1;\nroles a,b;\nstate n: int;\non accept from b when n -> begin;\non error from b -> send bye;\n").unwrap();
        let errs2 = check(&bad2).unwrap_err();
        assert!(errs2.iter().any(|e| e.message.contains("requires bool state 'n'")));
    }

    #[test]
    fn end_when_requires_bool_state() {
        let bad = parse("protocol p v1;\nroles a,b;\nstate n: int;\non query from a -> send inform to a;\non inform -> send bye;\nend when n;\n").unwrap();
        let errs = check(&bad).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("end when requires bool state 'n'")));
    }

    #[test]
    fn content_patterns_in_handlers() {
        let src = r#"
protocol p v1;
roles a, b;
on request {"task": "deploy*"} from b -> send propose to b;
on propose from a -> send bye;
"#;
        let p = parse(src).expect("content pattern parses");
        assert!(p.handlers[0].pattern_is_json(), "brace pattern detected");
        check(&p).expect("linear");
    }

    #[test]
    fn timers_parse() {
        let src = r#"
protocol p v1;
roles a, b;
state active: bool;
every 60s while active as a -> send progress to b;
on progress from a -> send bye;
"#;
        let p = parse(src).expect("timer parses");
        match &p.handlers[0].trigger {
            OnTrigger::Timer { every_ms, guard, as_role } => {
                assert_eq!(*every_ms, 60_000);
                assert!(guard.is_some());
                assert_eq!(as_role.as_deref(), Some("a"));
            }
            other => panic!("expected timer, got {other:?}"),
        }
        check(&p).expect("timer program is linear");
    }

    #[test]
    fn timer_needs_bool_guard_state() {
        let bad = parse("protocol p v1;\nroles a,b;\nstate n: int;\nevery 5s while n -> send progress to b;\non progress from b -> send bye;\n").unwrap();
        let errs = check(&bad).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("timer guard requires bool state 'n'")));
    }

    #[test]
    fn orphan_send_is_compile_error() {
        let src = "protocol p v1;\nroles a, b;\non query from a -> send inform to a;\n";
        let p = parse(src).unwrap();
        let errs = check(&p).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("unanswered send")),
            "linearity must catch orphan sends, got {errs:?}"
        );
        // adding the receiver fixes it
        let fixed = "protocol p v1;\nroles a, b;\non query -> send inform to a;\non inform from b -> send bye;\n";
        let p2 = parse(fixed).unwrap();
        check(&p2).expect("answered send compiles");
    }

    #[test]
    fn n_role_requires_full_attribution() {
        let three = "protocol t v1;\nroles a, b, c;\non query -> send inform to a;\non inform from b -> send bye;\n";
        let p = parse(three).unwrap();
        let errs = check(&p).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("3-role protocol")));
        // fully attributed 3-role program passes the n>2 rule
        let ok = "protocol t v1;\nroles a, b, c;\non query from a -> send inform to b;\non inform from c -> send bye;\n";
        let p2 = parse(ok).unwrap();
        check(&p2).expect("attributed n-role compiles (n>2 projection still phase-limited)");
    }

    #[test]
    fn liveness_warns_on_unreachable_terminal() {
        // request -> propose -> ??? : propose handler does only begin (no
        // outgoing send) — the chain cannot reach fulfil/bye/reject/error
        let src = "protocol p v1;\nroles a, b;\non query -> send inform to a;\non inform from b -> send bye;\non request from a -> send propose to a;\non propose from b -> begin;\n";
        let p = parse(src).unwrap();
        let warnings = check(&p).expect("compiles (warnings only)");
        assert!(
            warnings.iter().any(|w| w.contains("liveness")),
            "deadlock should be warned, got {warnings:?}"
        );
    }

    #[test]
    fn unterminated_block_and_missing_protocol() {
        assert!(parse("on query -> send inform;").is_err());
        assert!(parse("protocol p v1;\nroles a;\non query {").is_err());
        let e = parse("roles a;").unwrap_err();
        assert!(e.iter().any(|x| x.message.contains("missing 'protocol")));
    }
}
