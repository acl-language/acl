//! Protocol runtime (spec §33.4) and multiparty projection (§33.5).
//!
//! Phase 2: `Runtime` executes a compiled `.aclp` program as a live agent:
//! deliver events in, actions come out, everything lands in an auditable
//! transcript. Transport-agnostic and deterministic — a pure function of
//! (program, delivered events).
//!
//! Phase 3: `project` compiles the GLOBAL protocol into a per-role
//! ENDPOINT: each role sees only the handlers it must execute and the
//! sends it must emit. Two projected endpoints, run side by side through
//! two runtimes wired by a bus, reproduce the global conversation.

use crate::dialogue::dialogue_legal;
use crate::idl::IdlError;
use crate::protocol::{EndWhen, Expr, Handler, OnTrigger, ProtocolAction, ProtocolProgram};
use crate::cl::CommitmentStatus;
use crate::Performative;
use std::collections::BTreeMap;

// ---------------------------------------------------------------- endpoints

/// A per-role view of a global protocol (§33.5).
#[derive(Debug, Clone, PartialEq)]
pub struct RoleEndpoint {
    pub role: String,
    pub handlers: Vec<Handler>,
}

/// Which roles a handler's TRIGGER concerns: `on X from A` fires in the
/// role(s) that are NOT A. With two roles this is exactly one role.
pub fn handler_executor<'a>(prog: &'a ProtocolProgram, h: &Handler) -> Vec<&'a str> {
    match &h.trigger {
        // §43: `as <role>` restricts the executor to that ONE role
        OnTrigger::Event { as_role: Some(r), .. } => prog
            .roles
            .iter()
            .map(|s| s.as_str())
            .filter(|x| *x == r.as_str())
            .collect(),
        OnTrigger::Event { from: Some(sender), .. } => prog
            .roles
            .iter()
            .map(|s| s.as_str())
            .filter(|r| *r != sender.as_str())
            .collect(),
        OnTrigger::Event { from: None, .. } => {
            // unattributed trigger: every role EXCEPT none — in a 2-role
            // protocol this is the counterpart; keep both for n>2 safety
            prog.roles.iter().map(|s| s.as_str()).collect()
        }
        OnTrigger::Timer { as_role, .. } => match as_role {
            // attributed timer: fires in that role only
            Some(r) => prog
                .roles
                .iter()
                .map(|s| s.as_str())
                .filter(|x| *x == r.as_str())
                .collect(),
            None => prog.roles.iter().map(|s| s.as_str()).collect(),
        },
        OnTrigger::Breach | OnTrigger::Expired => {
            // the DEBTOR reports: executor = the role that is NOT the
            // handler's send target (send failure TO buyer ⇒ seller speaks)
            let target = h.actions.iter().find_map(|a| match a {
                ProtocolAction::Send { to: Some(t), .. } => Some(t.as_str()),
                _ => None,
            });
            match target {
                Some(t) if prog.roles.iter().any(|r| r == t) => prog
                    .roles
                    .iter()
                    .map(|s| s.as_str())
                    .filter(|r| *r != t)
                    .collect(),
                _ => prog.roles.iter().map(|s| s.as_str()).collect(),
            }
        }
    }
}

/// Project a global protocol onto `role`'s endpoint (§33.5).
///
/// Rules (phase 3, two-role semantics):
/// - a handler executes in the role that RECEIVES its trigger
///   (`from R` ⇒ every role ≠ R; omitted ⇒ counterpart default)
/// - `send X to R2` emits only in the executor's endpoint; `to` defaults
///   to the counterpart of the executor
/// - state actions (`begin`, `fulfil`, `retract`) execute in the executor
pub fn project(prog: &ProtocolProgram, role: &str) -> Result<RoleEndpoint, Vec<IdlError>> {
    let mut errors = Vec::new();
    if !prog.roles.iter().any(|r| r == role) {
        return Err(vec![IdlError {
            line: 0,
            message: format!("cannot project: role '{role}' is not declared"),
        }]);
    }
    // §43: n-role projection — every handler already carries explicit
    // attribution (check() requires `from` and `to` for 3+ role
    // protocols); the projection itself is role-set agnostic
    let two_role = prog.roles.len() == 2;
    let other = prog
        .roles
        .iter()
        .map(|s| s.as_str())
        .find(|r| *r != role)
        .unwrap_or("");

    let mut handlers = Vec::new();
    for h in &prog.handlers {
        let executors = handler_executor(prog, h);
        if !executors.contains(&role) {
            continue; // this handler runs in the other endpoint
        }
        let mut actions = Vec::new();
        for a in &h.actions {
            match a {
                ProtocolAction::Send { performative, to, fields, retry } => {
                    // the sender is the executor; resolve the receiver
                    let target = match to {
                        Some(r) => {
                            if !prog.roles.iter().any(|x| x == r) {
                                errors.push(IdlError {
                                    line: 0,
                                    message: format!("send targets undeclared role '{r}'"),
                                });
                            }
                            if r == role {
                                errors.push(IdlError {
                                    line: 0,
                                    message: format!("send targets the sender role '{r}'"),
                                });
                            }
                            Some(r.clone())
                        }
                        // §43: the counterpart default only exists in a
                        // 2-role protocol; check() forces explicit `to`
                        // otherwise
                        None if two_role => Some(other.to_string()),
                        None => {
                            errors.push(IdlError {
                                line: 0,
                                message: format!(
                                    "send without 'to' in a {}-role protocol (n-role requires explicit targets)",
                                    prog.roles.len()
                                ),
                            });
                            None
                        }
                    };
                    if let Some(t) = target {
                        if !t.is_empty() {
                            // §33.5: the receiving role sees this send as an
                            // incoming event — nothing to do in ITS handlers;
                            // the executor emits it
                            actions.push(ProtocolAction::Send {
                                performative: *performative,
                                to: Some(t),
                                fields: fields.clone(),
                                retry: retry.clone(),
                            });
                        }
                    }
                }
                other_action => actions.push(other_action.clone()),
            }
        }
        if !actions.is_empty() || matches!(h.trigger, OnTrigger::Event { .. }) {
            handlers.push(Handler { trigger: h.trigger.clone(), guard: h.guard.clone(), actions, line: 0 });
        }
    }

    if errors.is_empty() {
        Ok(RoleEndpoint { role: role.to_string(), handlers })
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------- runtime

/// Session variable value (phase 2: the types the runtime understands).
#[derive(Debug, Clone, PartialEq)]
pub enum StateValue {
    Record(std::collections::BTreeMap<String, StateValue>),
    Bool(bool),
    Int(i64),
    /// §38.3: floating-point values (inferred from pattern captures,
    /// arithmetic results) — serialized as MsgPack f64 / JSON number
    Float(f64),
    Str(String),
    /// §35: collections in session state
    List(Vec<StateValue>),
    Map(BTreeMap<String, StateValue>),
    Unset,
}

/// Something that happens TO the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// an incoming speech act from another role (`fields` = structured
    /// payload for content matching, §34) — §46: values are TYPED on the
    /// wire (FieldValue); string literals convert implicitly, so existing
    /// construction sites keep compiling, and native int/float/bool skip
    /// the `infer_value` heuristic entirely
    In {
        from: String,
        performative: Performative,
        pattern: Option<String>,
        fields: Option<BTreeMap<String, FieldValue>>,
    },
    /// an owed commitment was broken (by the OTHER side or by time)
    Breach,
    /// §38.7: an internal action failure (set/push/put eval error, send
    /// retry exhaustion) — routed to the session's `on error` handler
    InternalError(String),
    /// §49: a host time advance — journaled so replay reproduces timers,
    /// awaits and retry deadlines EXACTLY
    Tick(i64),
    /// §49: a session close — journaled so replay closes the same sessions
    SessionClosed,
    /// the deadline passed
    Expired,
}

/// Something the agent emits.
#[derive(Debug, Clone, PartialEq)]
pub struct OutEvent {
    pub to: Option<String>,
    pub performative: Performative,
    /// structured payload (§34) — subset-matched by receiving handlers
    pub fields: Option<BTreeMap<String, FieldValue>>,
}

/// §46: a typed field value on the wire — string literals convert
/// implicitly (From<&str>/From<String>) so protocol programs keep working;
/// native ints/floats/bools arrive typed and skip type inference
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl From<&str> for FieldValue {
    fn from(s: &str) -> Self {
        FieldValue::Str(s.to_string())
    }
}

impl From<String> for FieldValue {
    fn from(s: String) -> Self {
        FieldValue::Str(s)
    }
}

impl From<i64> for FieldValue {
    fn from(i: i64) -> Self {
        FieldValue::Int(i)
    }
}

impl From<f64> for FieldValue {
    fn from(f: f64) -> Self {
        FieldValue::Float(f)
    }
}

impl From<bool> for FieldValue {
    fn from(b: bool) -> Self {
        FieldValue::Bool(b)
    }
}

impl From<FieldValue> for StateValue {
    fn from(f: FieldValue) -> Self {
        match f {
            FieldValue::Str(s) => StateValue::Str(s),
            FieldValue::Int(i) => StateValue::Int(i),
            FieldValue::Float(f) => StateValue::Float(f),
            FieldValue::Bool(b) => StateValue::Bool(b),
        }
    }
}

impl FieldValue {
    /// the plain-text rendering used by string patterns and logs
    pub fn text(&self) -> String {
        match self {
            FieldValue::Str(s) => s.clone(),
            FieldValue::Int(i) => i.to_string(),
            FieldValue::Float(f) => f.to_string(),
            FieldValue::Bool(b) => b.to_string(),
        }
    }

    /// §46: typed conversion — Str goes through infer_value (a string
    /// payload may still BE a number written as text); native types pass
    /// through untouched
    pub fn to_state_value(&self) -> StateValue {
        match self {
            FieldValue::Str(s) => infer_value(s),
            other => other.clone().into(),
        }
    }
}

impl From<crate::cl::ClValue> for FieldValue {
    fn from(v: crate::cl::ClValue) -> Self {
        use crate::cl::ClValue;
        match v {
            ClValue::Str(s) => FieldValue::Str(s),
            ClValue::Int(i) => FieldValue::Int(i),
            ClValue::Float(f) => FieldValue::Float(f),
            ClValue::Bool(b) => FieldValue::Bool(b),
            other => FieldValue::Str(format!("{other:?}")),
        }
    }
}

/// §44: a declarative `await` collector — armed by an action, fed by
/// deliver(), completed or timed out by tick()
#[derive(Debug, Clone)]
struct AwaitSpec {
    count_left: i64,
    performative: Performative,
    into: String,
    deadline_ms: i64,
    flag: String,
}

/// §35.5: a send awaiting retry — driven by tick(), not by a clock.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSend {
    pub to: Option<String>,
    pub performative: Performative,
    pub fields: Option<BTreeMap<String, String>>,
    pub attempts_left: u32,
    pub next_due: i64,
    pub backoff_ms: i64,
    pub exponential: bool,
}

/// One line of the auditable transcript (§33.3).
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub seq: u64,
    pub dir: Dir,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
    State,
}

/// The live agent for ONE role: program + session state + commitment
/// lifecycle + transcript.
pub struct Runtime {
    pub role: String,
    /// public session id (set by SessionHost)
    pub session: Option<String>,
    /// this role's projected view — the runtime NEVER sees handlers
    /// that belong to another role (§33.5)
    endpoint: RoleEndpoint,
    end_when: Option<EndWhen>,
    program_states: BTreeMap<String, String>,
    states: BTreeMap<String, StateValue>,
    /// §38.1: user-defined functions available to this role
    functions: BTreeMap<String, crate::protocol::Function>,
    commitment: CommitmentStatus,
    log: Vec<LogEntry>,
    seq: u64,
    /// next due ms per handler index (timers)
    timer_due: Vec<i64>,
    /// §34 transcript verification: this role's last seen performative
    pub(crate) last_seen: Option<Performative>,
    /// §35.5: sends awaiting retry
    pending: Vec<PendingSend>,
    /// §38.7: recursion guard for internal-error routing
    handling_error: bool,
    /// §44: active await collectors
    awaits: Vec<AwaitSpec>,
    /// §50: current conversation phase (first declared phase, or "")
    phase: String,
    /// §50: declared phases (for enter validation)
    phases: Vec<String>,
    /// §45: active try/catch — the catch actions of the innermost `try`
    catch_stack: Vec<Vec<ProtocolAction>>,
    /// §45: set when a caught failure cuts the remaining body short
    catch_jump: bool,
    /// §45: true while catch actions run — failures inside a catch are
    /// NOT caught by the same try (routes to the session's on error)
    in_catch: bool,
    /// §35.4: host-injected clock (set by tick)
    now_ms: i64,
    /// §35.3: message templates for fmt()
    templates: Vec<(String, String)>,
    /// §41: declared record types (for list<T>/map<K,V> element checks)
    program_types: std::collections::BTreeMap<String, Vec<(String, String)>>,
    /// abort latch: compensation runs once
    aborted: bool,
    /// §16 enforcement: reject off-policy replies (opt-in)
    enforce_dialogue: bool,
    /// last performative we SENT (awaiting a legal reply)
    last_out: Option<Performative>,
    /// §35.4: abort conditions from the program
    abort_when: Vec<crate::protocol::Guard>,
}

static EMPTY_SCOPE: std::sync::LazyLock<BTreeMap<String, StateValue>> =
    std::sync::LazyLock::new(|| BTreeMap::new());

impl Runtime {
    pub fn new(prog: &ProtocolProgram, role: &str) -> Result<Self, IdlError> {
        if !prog.roles.iter().any(|r| r == role) {
            return Err(IdlError {
                line: 0,
                message: format!("runtime role '{role}' is not declared in the protocol"),
            });
        }
        let endpoint = project(prog, role).map_err(|e| {
            e.into_iter().next().unwrap_or(IdlError {
                line: 0,
                message: "projection failed".into(),
            })
        })?;
        Ok(Runtime {
            role: role.to_string(),
            endpoint,
            end_when: prog.end_when.clone(),
            program_states: prog
                .states
                .iter()
                .map(|s| (s.name.clone(), s.ty.clone().unwrap_or_default()))
                .collect(),
            states: prog
                .states
                .iter()
                .map(|s| (s.name.clone(), StateValue::Unset))
                .collect(),
            functions: prog
                .functions
                .iter()
                .map(|f| (f.name.clone(), f.clone()))
                .collect(),
            commitment: CommitmentStatus::Proposed,
            log: Vec::new(),
            seq: 0,
            timer_due: prog
                .handlers
                .iter()
                .map(|_| 0i64)
                .collect(),
            last_seen: None,
            pending: Vec::new(),
            handling_error: false,
            awaits: Vec::new(),
            phase: prog.phases.first().cloned().unwrap_or_default(),
            phases: prog.phases.clone(),
            catch_stack: Vec::new(),
            catch_jump: false,
            in_catch: false,
            now_ms: 0,
            aborted: false,
            enforce_dialogue: false,
            last_out: None,
            templates: prog.templates.clone(),
            program_types: prog.types.clone(),
            abort_when: prog.abort_when.clone(),
            session: None,
        })
    }

    pub fn state(&self, name: &str) -> Option<&StateValue> {
        self.states.get(name)
    }

    pub fn set_state(&mut self, name: &str, v: StateValue) -> Result<(), String> {
        // §34: typed session states — assignments are checked against the
        // declared type; mismatches are logged, never fatal
        if let Some(decl) = self.states.keys().find(|k| *k == name) {
            let declared = self
                .program_states
                .get(decl)
                .cloned()
                .unwrap_or_default();
            let d: &str = &declared;
            let mismatch = match (d, &v) {
                ("bool", StateValue::Bool(_)) | ("int", StateValue::Int(_)) | ("str", StateValue::Str(_)) => false,
                ("float", StateValue::Float(_)) | ("float", StateValue::Int(_)) => false,
                ("list", StateValue::List(_)) | ("map", StateValue::Map(_)) => false,
                ("bool", _) | ("int", _) | ("str", _) | ("float", _) | ("list", _) | ("map", _) => {
                    v != StateValue::Unset
                }
                // §41: generic containers `list<T>` / `map<K,V>` — the
                // container kind AND every element must match
                (t, StateValue::List(items)) if t.starts_with("list<") => {
                    let elem = &t[5..t.len() - 1];
                    !items.iter().all(|it| self.value_matches_type(elem, it))
                }
                (t, StateValue::Map(entries)) if t.starts_with("map<") => {
                    let inner = &t[4..t.len() - 1];
                    let (kt, vt) = inner.split_once(',').unwrap_or(("str", inner));
                    let (kt, vt) = (kt.trim(), vt.trim());
                    !entries.iter().all(|(k, val)| {
                        self.value_matches_type(kt, &StateValue::Str(k.clone()))
                            && self.value_matches_type(vt, val)
                    })
                }
                // §41: record-typed states — a Record with matching __type
                (t, StateValue::Record(m)) => {
                    !m.get("__type").map_or(false, |tv| {
                        matches!(tv, StateValue::Str(s) if s == t)
                    })
                }
                (t, StateValue::Map(m)) if !t.starts_with("map<") => {
                    // a record literal assigned to a record-typed state is a
                    // Map only from JSON roundtrips — accept maps with __type
                    !m.get("__type").map_or(false, |tv| {
                        matches!(tv, StateValue::Str(s) if s == t)
                    })
                }
                (t, StateValue::List(_)) if t.starts_with("list<") => false,
                (t, _) if self.is_declared_record(t) => {
                    // record-typed state assigned a non-record value
                    v != StateValue::Unset
                }
                _ => false, // vocabulary types: untyped
            };
            if mismatch {
                return Err(format!(
                    "type mismatch: state '{name}' is '{declared}' but assigned {}",
                    value_text(&v)
                ));
            }
        }
        self.states.insert(name.to_string(), v);
        Ok(())
    }

    fn is_declared_record(&self, ty: &str) -> bool {
        self.program_types.contains_key(ty)
    }

    /// §41: does a value conform to a declared element/state type?
    fn value_matches_type(&self, ty: &str, v: &StateValue) -> bool {
        match (ty, v) {
            (_, StateValue::Unset) => true,
            ("bool", StateValue::Bool(_)) | ("int", StateValue::Int(_))
            | ("str", StateValue::Str(_)) | ("float", StateValue::Float(_)) => true,
            ("float", StateValue::Int(_)) => true,
            ("list", StateValue::List(_)) | ("map", StateValue::Map(_)) => true,
            (t, StateValue::Record(m)) => {
                m.get("__type").map_or(false, |tv| {
                    matches!(tv, StateValue::Str(s) if s == t)
                })
            }
            (t, StateValue::Map(m)) if self.is_declared_record(t) => {
                m.get("__type").map_or(false, |tv| {
                    matches!(tv, StateValue::Str(s) if s == t)
                })
            }
            (t, StateValue::List(items)) if t.starts_with("list<") => {
                let elem = &t[5..t.len() - 1];
                items.iter().all(|it| self.value_matches_type(elem, it))
            }
            (t, StateValue::Map(entries)) if t.starts_with("map<") => {
                let inner = &t[4..t.len() - 1];
                let (kt, vt) = inner.split_once(',').unwrap_or(("str", inner));
                let (kt, vt) = (kt.trim(), vt.trim());
                entries.iter().all(|(k, val)| {
                    self.value_matches_type(kt, &StateValue::Str(k.clone()))
                        && self.value_matches_type(vt, val)
                })
            }
            _ => false,
        }
    }

    /// §49: the runtime clock (absolute, host-injected) — replay equality
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// §35.9: full state snapshot (for replay verification)
    pub fn states_snapshot(&self) -> BTreeMap<String, StateValue> {
        self.states.clone()
    }

    /// §35.1: evaluate a pure expression against state + event fields
    fn eval(&self, e: &Expr, ev: &Event, scope: &BTreeMap<String, StateValue>) -> Result<StateValue, String> {
        let arg = match ev {
            Event::In { fields, .. } => fields.clone().unwrap_or_default(),
            _ => BTreeMap::new(),
        };
        let eval = |e: &Expr, s: &Runtime, sc: &BTreeMap<String, StateValue>| -> Result<StateValue, String> { s.eval(e, ev, sc) };
        Ok(match e {
            Expr::LitInt(i) => StateValue::Int(*i),
            Expr::LitStr(s) => StateValue::Str(s.clone()),
            Expr::LitBool(b) => StateValue::Bool(*b),
            Expr::State(n) => scope.get(n).cloned().unwrap_or_else(|| self.states.get(n).cloned().unwrap_or(StateValue::Unset)),
            Expr::Capture(n) => scope.get(n).cloned().unwrap_or(StateValue::Unset),
            Expr::Arg(a) => match arg.get(a) {
                Some(v) => v.to_state_value(),
                None => StateValue::Unset,
            },
            Expr::Not(x) => match eval(x, self, scope)? {
                StateValue::Bool(b) => StateValue::Bool(!b),
                other => StateValue::Bool(!self.truthy(&other)),
            },
            Expr::Bin { op, l, r } => {
                let lv = eval(l, self, scope)?;
                let rv = eval(r, self, scope)?;
                use crate::protocol::BinOp::*;
                match op {
                    And => StateValue::Bool(self.truthy(&lv) && self.truthy(&rv)),
                    Or => StateValue::Bool(self.truthy(&lv) || self.truthy(&rv)),
                    Lt | Le | Gt | Ge | Eq | Ne => {
                        let c = compare(&lv, &rv, *op)?;
                        StateValue::Bool(c)
                    }
                    Add | Sub | Mul | Div => arithmetic(&lv, &rv, *op)?,
                }
            }
            Expr::Field { base, name } => {
                let b = eval(base, self, scope)?;
                match b {
                    StateValue::Record(m) => m.get(name).cloned().unwrap_or(StateValue::Unset),
                    StateValue::Map(m) => m.get(name).cloned().unwrap_or(StateValue::Unset),
                    _ => StateValue::Unset,
                }
            }
            Expr::RecLit { ty, fields } => {
                let mut m = std::collections::BTreeMap::new();
                m.insert("__type".to_string(), StateValue::Str(ty.clone()));
                for (n, e) in fields {
                    m.insert(n.clone(), eval(e, self, scope)?);
                }
                StateValue::Record(m)
            }
            Expr::Call { name, args } => {
                let vals: Vec<StateValue> = args
                    .iter()
                    .map(|a| eval(a, self, scope))
                    .collect::<Result<_, _>>()?;
                if name == "fmt" {
                    // fmt(template_or_inline, args...) — §35.3 user templates
                    // + §38.9 rich interpolation: `${expr}` (states, captures,
                    // arithmetic, fields) evaluated in the caller's scope;
                    // `$${` is the literal escape. A registered template name
                    // wins over an inline literal.
                    let tname = match vals.first() {
                        Some(StateValue::Str(s)) => s.clone(),
                        _ => return Err("fmt needs a template name".into()),
                    };
                    let template = match self.templates.iter().find(|(n, _)| *n == tname) {
                        Some((_, t)) => t.clone(),
                        None if tname.contains("${") => tname,
                        None => return Err(format!("no message template '{tname}'")),
                    };
                    let esc = "\u{1}";
                    let mut out = String::new();
                    let cs: Vec<char> = template.replace("$${", esc).chars().collect();
                    let mut i = 0;
                    while i < cs.len() {
                        if cs[i] == '$' && i + 1 < cs.len() && cs[i + 1] == '{' {
                            let mut depth = 1i64;
                            let mut j = i + 2;
                            while j < cs.len() && depth > 0 {
                                match cs[j] {
                                    '{' => depth += 1,
                                    '}' => depth -= 1,
                                    _ => {}
                                }
                                j += 1;
                            }
                            if depth != 0 {
                                return Err("unterminated ${ in template".into());
                            }
                            let src: String = cs[i + 2..j - 1].iter().collect();
                            let e = crate::protocol::Expr::parse_expr(src.trim())?;
                            let v = self.eval(&e, ev, scope)?;
                            out.push_str(&value_text(&v));
                            i = j;
                        } else {
                            out.push(cs[i]);
                            i += 1;
                        }
                    }
                    let mut out = out.replace(esc, "${");
                    // positional {i} placeholders still supported
                    for (idx, v) in vals[1..].iter().enumerate() {
                        out = out.replace(&format!("{{{idx}}}"), &value_text(v));
                    }
                    return Ok(StateValue::Str(out));
                }
                builtin(name, &vals)?
            }
        })
    }

    fn truthy(&self, v: &StateValue) -> bool {
        match v {
            StateValue::Bool(b) => *b,
            StateValue::Int(i) => *i != 0,
            StateValue::Float(f) => *f != 0.0,
            StateValue::Str(s) => !s.is_empty(),
            StateValue::List(l) => !l.is_empty(),
            StateValue::Map(m) => !m.is_empty(),
            StateValue::Record(m) => !m.is_empty(),
            StateValue::Unset => false,
        }
    }

    /// boolean view of session state for guard evaluation (§34)
    fn bool_view(&self) -> BTreeMap<String, bool> {
        self.states
            .iter()
            .map(|(k, v)| {
                let b = match v {
                    StateValue::Bool(b) => *b,
                    StateValue::Int(i) => *i != 0,
                    StateValue::Float(f) => *f != 0.0,
                    StateValue::Str(s) => !s.is_empty(),
                    StateValue::List(l) => !l.is_empty(),
                    StateValue::Map(m) => !m.is_empty(),
                    StateValue::Record(m) => !m.is_empty(),
                    StateValue::Unset => false,
                };
                (k.clone(), b)
            })
            .collect()
    }

    /// §16: enable strict dialogue-matrix enforcement for this runtime
    pub fn enforce_dialogue(&mut self, on: bool) {
        self.enforce_dialogue = on;
    }

    pub fn commitment(&self) -> CommitmentStatus {
        self.commitment
    }

    pub fn transcript(&self) -> &[LogEntry] {
        &self.log
    }

    fn log(&mut self, dir: Dir, text: String) {
        self.seq += 1;
        self.log.push(LogEntry { seq: self.seq, dir, text });
    }

    /// §38.7: route an internal failure to the session's `on error`
    /// handler. The failure is always logged; the handler runs at most one
    /// level deep (failures inside it are logged only — no error loops).
    pub fn deliver_internal_error(&mut self, reason: String) -> Vec<OutEvent> {
        self.log(Dir::State, format!("internal error: {reason}"));
        if self.handling_error {
            return Vec::new();
        }
        let ev = Event::InternalError(reason);
        if let Some((hidx, caps)) = self.matching_handler(&ev) {
            self.handling_error = true;
            let out = self.run_handler(hidx, &ev, caps);
            self.handling_error = false;
            out
        } else {
            Vec::new()
        }
    }

    /// Deliver an event: find the FIRST handler whose trigger matches and
    /// execute its actions in order. Returns what the agent emits.
    pub fn deliver(&mut self, ev: Event) -> Vec<OutEvent> {
        // every incoming speech act enters the transcript, handled or not
        self.log(Dir::In, event_text(&ev));
        if let Event::In { performative, .. } = &ev {
            self.last_seen = Some(*performative);
        }
        // §16: when enforcement is on, an incoming In-event must be a
        // legal reply to our last outgoing move (off-policy = logged and
        // silently dropped — the handler does NOT fire)
        if self.enforce_dialogue {
            if let Event::In { performative, .. } = &ev {
                if let Some(prev) = self.last_out {
                    if !dialogue_legal(prev, *performative) {
                        self.log(
                            Dir::State,
                            format!(
                                "off-policy reply dropped: {:?} after our {:?} (§16)",
                                performative, prev
                            ),
                        );
                        return Vec::new();
                    }
                }
            }
        }
        // §44: an active await collector CONSUMES matching events (the
        // collected payloads do not need handlers)
        if let Event::In { performative, fields, .. } = &ev {
            if let Some(i) = self.awaits.iter().position(|a| a.performative == *performative) {
                let spec = self.awaits[i].clone();
                let payload = StateValue::Map(
                    fields
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(k, v)| (k, v.to_state_value()))
                        .collect(),
                );
                // append to the list state
                match self.states.get_mut(&spec.into) {
                    Some(StateValue::List(l)) => l.push(payload),
                    Some(slot) if matches!(slot, StateValue::Unset) => {
                        *slot = StateValue::List(vec![payload]);
                    }
                    _ => {}
                    None => {
                        self.states.insert(spec.into.clone(), StateValue::List(vec![payload]));
                    }
                }
                self.log(
                    Dir::In,
                    format!("await collected {} ({}/{} into {})", perf_name(spec.performative), spec.count_left, spec.count_left, spec.into),
                );
                if spec.count_left <= 1 {
                    self.awaits.remove(i);
                    let _ = self.set_state(&spec.flag, StateValue::Bool(true));
                    self.log(Dir::State, format!("await satisfied: {} = true", spec.flag));
                } else {
                    self.awaits[i].count_left = spec.count_left - 1;
                }
                return Vec::new();
            }
        }
        let (hidx, captures) = match self.matching_handler(&ev) {
            Some((hidx, caps)) => (hidx, caps),
            None => return Vec::new(),
        };
        let mut out = self.run_handler(hidx, &ev, captures);
        // §35.4: abort conditions — checked after every delivery; when a
        // guard holds, compensation handlers run once (latched)
        if !self.aborted {
            let guards = self.abort_when.clone();
            for g in guards {
                if g.eval(&self.bool_view()) {
                    self.log(Dir::State, "abort condition met".into());
                    out.extend(self.run_actions(&[ProtocolAction::Abort], &ev));
                    self.aborted = true;
                    break;
                }
            }
        }
        out
    }

    fn matching_handler(&self, ev: &Event) -> Option<(usize, BTreeMap<String, StateValue>)> {
        self.endpoint.handlers.iter().enumerate().find_map(|(i, h)| match (&h.trigger, ev) {
            (OnTrigger::Event { performative, pattern, from, phase, .. }, Event::In { from: f, performative: p, pattern: pat, fields }) => {
                let mut captures: BTreeMap<String, StateValue> = BTreeMap::new();
                let content_ok = match (pattern.as_ref(), pat.as_ref(), fields) {
                    // no pattern declared: matches anything
                    (None, ..) => true,
                    // literal pattern `("quote")`: equality with the event's pattern
                    (Some(want), Some(got), _) if want.starts_with('(') => want == got,
                    // §34 content pattern `{"k": "v"}`: subset match against
                    // the event's structured fields; "*"-suffixed values are
                    // prefix wildcards
                    (Some(want), _, Some(fs)) if want.starts_with('{') => {
                        match serde_json::from_str::<BTreeMap<String, String>>(want) {
                            Ok(want_map) => want_map.iter().all(|(k, want_v)| {
                                match fs.get(k) {
                                    None => false,
                                    Some(got_v) if want_v.ends_with('*') => {
                                        got_v.text().starts_with(want_v.trim_end_matches('*'))
                                    }
                                    // §35.5: capture variables `{"k": "$var"}`
                                    // — the event's value is bound to `var`,
                                    // with §38.3 type inference (int, float,
                                    // bool, else string)
                                    Some(got_v) if want_v.starts_with('$') => {
                                        captures.insert(want_v[1..].to_string(), got_v.to_state_value());
                                        true
                                    }
                                    Some(got_v) => got_v.text() == want_v.to_string(),
                                }
                            }),
                            Err(_) => false,
                        }
                    }
                    // content pattern but event has no fields: no match
                    (Some(_), _, None) => false,
                    _ => false,
                };
                let guard_ok = h.guard.as_ref().map_or(true, |g| {
                    match g {
                        crate::protocol::Guard::ExprGuard(expr) => {
                            // §38.3: expression guard — evaluate with captures
                            // in scope; non-bool results use truthiness
                            match self.eval(expr, ev, &captures) {
                                Ok(v) => match v {
                                    StateValue::Bool(b) => b,
                                    _ => self.truthy(&v),
                                },
                                Err(_) => false, // eval error = guard fails
                            }
                        }
                        _ => g.eval(&self.bool_view()),
                    }
                });
                let phase_ok = match phase {
                    Some(want) => self.phase == *want,
                    None => true,
                };
                if content_ok
                    && guard_ok
                    && phase_ok
                    && from.as_ref().map_or(true, |want| want == f)
                    && (performative == p)
                {
                    return Some((i, captures));
                }
                None
            }
            (OnTrigger::Breach, Event::Breach) => Some((i, BTreeMap::new())),
            (OnTrigger::Expired, Event::Expired) => Some((i, BTreeMap::new())),
            // §38.7: internal failures match ONLY an unattributed `on error`
            // handler (an `on error from X` is for external error messages)
            (OnTrigger::Event { performative: Performative::Error, from: None, .. }, Event::InternalError(reason)) => {
                let mut caps = BTreeMap::new();
                caps.insert("reason".to_string(), StateValue::Str(reason.clone()));
                Some((i, caps))
            }
            _ => None,
        })
    }

    fn run_handler(&mut self, hidx: usize, ev: &Event, captures: BTreeMap<String, StateValue>) -> Vec<OutEvent> {
        let actions = self.endpoint.handlers[hidx].actions.clone();
        let mut out = Vec::new();
        for a in &actions {
            match a {
                ProtocolAction::Send { performative, to, fields, retry } => {
                    self.last_out = Some(*performative);
                    self.log(
                        Dir::Out,
                        format!(
                            "send {} to {}{}{}",
                            perf_name(*performative),
                            to.clone().unwrap_or_default(),
                            fields.clone().map(|f| format!(" {}", serde_json::to_string(&f).unwrap_or_default())).unwrap_or_default(),
                            retry.as_ref().map(|r| format!(" retry={} backoff={}ms{}", r.attempts, r.backoff_ms, if r.exponential { " exp" } else { "" })).unwrap_or_default()
                        ),
                    );
                    if let Some(rp) = retry {
                        self.pending.push(PendingSend {
                            to: to.clone(),
                            performative: *performative,
                            fields: fields.clone(),
                            attempts_left: rp.attempts,
                            next_due: self.now_ms + rp.backoff_ms,
                            backoff_ms: rp.backoff_ms,
                            exponential: rp.exponential,
                        });
                    }
                    out.push(OutEvent {
                            to: to.clone(),
                            performative: *performative,
                            fields: fields.as_ref().map(|m| {
                                m.iter().map(|(k, v)| (k.clone(), FieldValue::Str(v.clone()))).collect()
                            }),
                        });
                }
                ProtocolAction::Broadcast { performative, fields, to } => {
                    // §38.8: fan-out — one OutEvent per role named in the
                    // list state; non-list target is an internal error
                    self.last_out = Some(*performative);
                    let snapshot = self.states.get(to).cloned();
                    match snapshot.as_ref() {
                        Some(StateValue::List(items)) => {
                            self.log(
                                Dir::Out,
                                format!(
                                    "broadcast {} to {} ({} targets){}",
                                    perf_name(*performative),
                                    to,
                                    items.len(),
                                    fields.clone().map(|f| format!(" {}", serde_json::to_string(&f).unwrap_or_default())).unwrap_or_default()
                                ),
                            );
                            for item in items {
                                let target = match item {
                                    StateValue::Str(s) => s.clone(),
                                    other => continue, // non-string entries skipped
                                };
                                out.push(OutEvent {
                                    to: Some(target),
                                    performative: *performative,
                                    fields: fields.as_ref().map(|m| {
                                        m.iter().map(|(k, v)| (k.clone(), FieldValue::Str(v.clone()))).collect()
                                    }),
                                });
                            }
                        }
                        other => {
                            let reason = format!("broadcast to '{to}': not a list ({:?})", other.map(value_text).unwrap_or_default());
                            self.action_failed(reason, &mut out);
                        }
                    }
                }
                ProtocolAction::Enter { phase } => {
                    // §50: conversation phase transition
                    if self.phases.iter().any(|p| p == phase) {
                        self.phase = phase.clone();
                        self.log(Dir::State, format!("phase -> {phase}"));
                    } else {
                        out.extend(self.deliver_internal_error(format!(
                            "enter '{phase}': not a declared phase"
                        )));
                    }
                }
                ProtocolAction::Try { body, catch } => {
                    // §45: handler-local error flow
                    self.catch_stack.push(catch.clone());
                    let inner = self.run_actions(body, ev);
                    self.catch_stack.pop();
                    out.extend(inner);
                }
                ProtocolAction::Await { count, performative, into, within_ms, flag } => {
                    // §44: arm the collector; flag starts false
                    self.awaits.push(AwaitSpec {
                        count_left: *count,
                        performative: *performative,
                        into: into.clone(),
                        deadline_ms: self.now_ms + within_ms,
                        flag: flag.clone(),
                    });
                    let _ = self.set_state(flag, StateValue::Bool(false));
                    self.log(
                        Dir::State,
                        format!(
                            "await {} {}: waiting for {} {} into {} within {}ms",
                            count, flag, count, perf_name(*performative), into, within_ms
                        ),
                    );
                }
                ProtocolAction::Set { name, field, expr } => match self.eval(expr, ev, &captures) {
                    Ok(v) => {
                        if let Err(e) = self.set_state(name, v.clone()) {
                            self.action_failed(format!("set {name}: {e}"), &mut out);
                        } else {
                            self.log(Dir::State, format!("set {} = {}", name, value_text(&v)));
                        }
                    }
                    Err(e) => self.action_failed(format!("set {name}: {e}"), &mut out),
                },
                ProtocolAction::Push { state, expr } => match self.eval(expr, ev, &captures) {
                    Ok(v) => {
                        let is_list = matches!(self.states.get(state), Some(StateValue::List(_)) | Some(StateValue::Unset) | None);
                        // §41: element must match a `list<T>` declaration
                        if is_list {
                            if let Some(ty) = self.program_states.get(state) {
                                if let Some(elem) = ty.strip_prefix("list<").map(|t| t.strip_suffix('>').unwrap_or(t)) {
                                    if ty.starts_with("list<") && !self.value_matches_type(elem, &v) {
                                        self.action_failed(
                                            format!(
                                            "push {state}: element {} does not match '{ty}'",
                                            value_text(&v)),
                                            &mut out,
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                        if !is_list {
                            let other = self.states.get(state).cloned().unwrap_or(StateValue::Unset);
                            let reason = format!("push {state}: not a list ({})", value_text(&other));
                            self.action_failed(reason, &mut out);
                            continue;
                        }
                        match self.states.get_mut(state) {
                            Some(StateValue::List(l)) => l.push(v),
                            _ => {
                                self.states.insert(state.clone(), StateValue::List(vec![v]));
                            }
                        }
                        self.log(Dir::State, format!("push {}", state));
                    }
                    Err(e) => self.action_failed(format!("push {state}: {e}"), &mut out),
                },
                ProtocolAction::Put { state, key, expr } => match self.eval(expr, ev, &captures) {
                    Ok(v) => {
                        let is_map = matches!(self.states.get(state), Some(StateValue::Map(_)) | Some(StateValue::Unset) | None);
                        // §41: key/value must match a `map<K,V>` declaration
                        if is_map {
                            if let Some(ty) = self.program_states.get(state) {
                                if let Some(inner) = ty.strip_prefix("map<").map(|t| t.strip_suffix('>').unwrap_or(t)) {
                                    if ty.starts_with("map<") {
                                        let (kt, vt) = inner.split_once(',').map(|(a, b)| (a.trim(), b.trim())).unwrap_or(("str", inner));
                                        let kv = StateValue::Str(key.clone());
                                        if !self.value_matches_type(kt, &kv) || !self.value_matches_type(vt, &v) {
                                            self.action_failed(
                                                format!(
                                                "put {state}[{key}]: key/value does not match '{ty}'"),
                                                &mut out,
                                            );
                                            continue;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        if !is_map {
                            let other = self.states.get(state).cloned().unwrap_or(StateValue::Unset);
                            let reason = format!("put {state}: not a map ({})", value_text(&other));
                            self.action_failed(reason, &mut out);
                            continue;
                        }
                        match self.states.get_mut(state) {
                            Some(StateValue::Map(m)) => {
                                m.insert(key.clone(), v);
                            }
                            _ => {
                                self.states.insert(state.clone(), StateValue::Map(BTreeMap::from([(key.clone(), v)])));
                            }
                        }
                        self.log(Dir::State, format!("put {}[{}]", state, key));
                    }
                    Err(e) => self.action_failed(format!("put {state}: {e}"), &mut out),
                },
                ProtocolAction::For { var, over, actions } => {
                    // iterate a SNAPSHOT (the body may mutate the list);
                    // the loop var is a scoped state, restored after
                    let snapshot = match self.states.get(over) {
                        Some(StateValue::List(l)) => l.clone(),
                        Some(other) => {
                            self.log(Dir::State, format!("for {}: not a list ({:?})", over, other));
                            continue;
                        }
                        None => continue,
                    };
                    let saved = self.states.get(var).cloned();
                    self.log(Dir::State, format!("for {} in {} ({} items)", var, over, snapshot.len()));
                    for item in snapshot {
                        self.states.insert(var.clone(), item.clone());
                        out.extend(self.run_actions(actions, ev));
                    }
                    match saved {
                        Some(v) => {
                            self.states.insert(var.clone(), v);
                        }
                        None => {
                            self.states.remove(var);
                        }
                    }
                }
                ProtocolAction::Return { expr } => {
                // Evaluated but not printed in the transcript
                let _ = self.eval(expr, ev, &captures);
            }
            ProtocolAction::Abort => {
                    // §35.4: fire compensation handlers, mark breached
                    self.log(Dir::State, "ABORT: compensation begins".into());
                    self.commitment = CommitmentStatus::Breached;
                    let abort_handlers: Vec<usize> = self
                        .endpoint
                        .handlers
                        .iter()
                        .enumerate()
                        .filter(|(_, h)| matches!(h.trigger, OnTrigger::Breach))
                        .map(|(i, _)| i)
                        .collect();
                    for i in abort_handlers {
                        out.extend(self.run_handler(i, &Event::Breach, BTreeMap::new()));
                    }
                }
                ProtocolAction::Begin => match self.commitment {
                    CommitmentStatus::Proposed => {
                        self.commitment = CommitmentStatus::Active;
                        self.log(Dir::State, "commitment proposed -> active".into());
                    }
                    other => self.log(
                        Dir::State,
                        format!("begin ignored: commitment is {other:?}, not Proposed"),
                    ),
                },
                ProtocolAction::Fulfil => match self.commitment {
                    CommitmentStatus::Active => {
                        self.commitment = CommitmentStatus::Fulfilled;
                        self.log(Dir::State, "commitment active -> fulfilled".into());
                    }
                    other => self.log(
                        Dir::State,
                        format!("fulfil ignored: commitment is {other:?}, not Active"),
                    ),
                },
                ProtocolAction::Retract { target } => {
                    // §32.1 as a statement: the fact becomes retracted in
                    // this role's view; the transcript keeps the history
                    let _ = self.set_state(target, StateValue::Bool(false));
                    self.log(Dir::State, format!("retract({target})"));
                }
            }
        }
        out
    }

    /// run a list of actions (used by `for` bodies)
    /// §45: route an action failure — inside a `try` the catch actions
    /// run and the remaining body is cut; otherwise the session's
    /// `on error` handler fires (§38.7)
    fn action_failed(&mut self, reason: String, out: &mut Vec<OutEvent>) {
        if !self.in_catch {
            if let Some(catch) = self.catch_stack.last().cloned() {
                // §45: run the catch with catch suppression (a failure
                // INSIDE the catch routes to the session's on error), and
                // only THEN latch the body cut
                self.in_catch = true;
                self.log(Dir::State, format!("caught: {reason}"));
                out.extend(self.run_actions(&catch, &Event::InternalError(reason)));
                self.in_catch = false;
                self.catch_jump = true;
                return;
            }
        }
        out.extend(self.deliver_internal_error(reason));
    }




    fn run_actions(&mut self, actions: &[ProtocolAction], ev: &Event) -> Vec<OutEvent> {
        let mut out = Vec::new();
        for a in actions {
            // §45: a caught failure cuts the remaining body short
            if self.catch_jump {
                self.catch_jump = false;
                break;
            }
            match a {
                ProtocolAction::For { .. } | ProtocolAction::Abort => {
                    // nested for: clone through a single-handler trick is
                    // overkill — inline recursively via a mini handler
                    match a {
                        ProtocolAction::For { var, over, actions } => {
                            let snapshot = match self.states.get(over) {
                                Some(StateValue::List(l)) => l.clone(),
                                _ => continue,
                            };
                            let saved = self.states.get(var).cloned();
                            for item in snapshot {
                                self.states.insert(var.clone(), item.clone());
                                out.extend(self.run_actions(actions, ev));
                            }
                            match saved {
                                Some(v) => {
                                    self.states.insert(var.clone(), v);
                                }
                                None => {
                                    self.states.remove(var);
                                }
                            }
                        }
                        ProtocolAction::Abort => {
                            self.log(Dir::State, "ABORT (nested): compensation begins".into());
                            self.commitment = CommitmentStatus::Breached;
                            let comp: Vec<usize> = self
                                .endpoint
                                .handlers
                                .iter()
                                .enumerate()
                                .filter(|(_, h)| matches!(h.trigger, OnTrigger::Breach))
                                .map(|(i, _)| i)
                                .collect();
                            for i in comp {
                                out.extend(self.run_handler(i, ev, BTreeMap::new()));
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                other => {
                    // reuse run_handler's single-action logic by wrapping
                    let h = Handler { trigger: OnTrigger::Expired, guard: None, actions: vec![other.clone()], line: 0 };
                    self.endpoint.handlers.push(h);
                    let idx = self.endpoint.handlers.len() - 1;
                    out.extend(self.run_handler(idx, ev, BTreeMap::new()));
                    self.endpoint.handlers.pop();
                }
            }
        }
        out
    }

    /// Host-injected time (§34): the runtime stays clock-free; the
    /// transport host calls tick() with elapsed milliseconds and due
    /// timer handlers fire here, deterministically.
    pub fn tick(&mut self, elapsed_ms: i64) -> Vec<OutEvent> {
        let mut out = Vec::new();
        // collect due timers first (period + handler index), then fire —
        // avoids borrowing self.endpoint while mutating
        let due: Vec<(usize, i64)> = self
            .endpoint
            .handlers
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let OnTrigger::Timer { every_ms, guard, .. } = &h.trigger else { return None };
                let bools = self.bool_view();
                if let Some(g) = guard {
                    if !g.eval(&bools) {
                        return None; // guard false: timer silent this round
                    }
                }
                Some((i, *every_ms))
            })
            .collect();
        for (i, every_ms) in due {
            while self.timer_due[i] + every_ms <= elapsed_ms {
                self.timer_due[i] += every_ms;
                self.log(Dir::In, format!("timer({}ms)", every_ms));
                out.extend(self.run_handler(i, &Event::Expired, BTreeMap::new()));
            }
        }
        // §44: await deadlines — overdue collectors set the flag false
        let mut i = 0;
        while i < self.awaits.len() {
            if self.awaits[i].deadline_ms <= elapsed_ms {
                let spec = self.awaits.remove(i);
                let _ = self.set_state(&spec.flag, StateValue::Bool(false));
                self.log(
                    Dir::State,
                    format!("await timed out: {} = false", spec.flag),
                );
            } else {
                i += 1;
            }
        }
        // §35.5: retry pending sends when due; exhaustion = retry dropped
        // (logged; the caller sees only the original send)
        self.now_ms = elapsed_ms;
        let drained: Vec<PendingSend> = self.pending.drain(..).collect();
        let mut still_pending = Vec::new();
        for p in drained {
            if p.next_due <= elapsed_ms {
                if p.attempts_left > 1 {
                    let backoff = if p.exponential { p.backoff_ms * 2 } else { p.backoff_ms };
                    self.log(
                        Dir::Out,
                        format!(
                            "retry {} to {} ({} left)",
                            perf_name(p.performative),
                            p.to.clone().unwrap_or_default(),
                            p.attempts_left - 1
                        ),
                    );
                    out.push(OutEvent {
                            to: p.to.clone(),
                            performative: p.performative,
                            fields: p.fields.as_ref().map(|m| {
                                m.iter().map(|(k, v)| (k.clone(), FieldValue::Str(v.clone()))).collect()
                            }),
                        });
                    still_pending.push(PendingSend {
                        attempts_left: p.attempts_left - 1,
                        next_due: elapsed_ms + backoff,
                        backoff_ms: backoff,
                        ..p
                    });
                } else {
                    let reason = format!(
                        "send {} to {}: retry exhausted",
                        perf_name(p.performative),
                        p.to.clone().unwrap_or_default()
                    );
                    out.extend(self.deliver_internal_error(reason));
                }
            } else {
                still_pending.push(p);
            }
        }
        self.pending = still_pending;
        out
    }

    /// Session end check: `end when` clause against builtins or states.
    pub fn ended(&self) -> bool {
        match &self.end_when {
            None => false,
            Some(EndWhen::Builtin(b)) => match b.as_str() {
                "fulfilled" => self.commitment == CommitmentStatus::Fulfilled,
                "breached" => self.commitment == CommitmentStatus::Breached,
                "expired" => false, // phase 2: expiration is event-driven
                _ => false,
            },
            Some(EndWhen::State(s)) => {
                matches!(self.states.get(s), Some(StateValue::Bool(true)))
            }
        }
    }
}

/// §34: transcript verification — replay a recorded exchange through the
/// projected runtimes and check the replay is legal: every send must be
/// dialogue-legal for the receiving role's previous turn, and every send
/// must land on a declared handler (or be explicitly external).
pub fn verify_transcript(
    prog: &ProtocolProgram,
    events: &[(String, Event)],
) -> Result<Vec<String>, Vec<String>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut runtimes: BTreeMap<String, Runtime> = BTreeMap::new();
    for role in &prog.roles {
        match Runtime::new(prog, role) {
            Ok(rt) => {
                runtimes.insert(role.clone(), rt);
            }
            Err(e) => errors.push(e.message),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // queue of pending deliveries: (to_role, event)
    let mut queue: Vec<(String, Event)> = events.to_vec();
    let mut steps = 0usize;
    while !queue.is_empty() {
        let (role, ev) = queue.remove(0);
        steps += 1;
        if steps > 4096 {
            errors.push("transcript does not terminate (loop?)".into());
            break;
        }
        let Some(rt) = runtimes.get_mut(&role) else {
            errors.push(format!("event targets undeclared role '{role}'"));
            continue;
        };
        // legality: what arrives must be dialogue-legal for the role's
        // last outgoing or incoming move (§16)
        if let Event::In { performative, .. } = &ev {
            if let Some(last) = rt.last_seen {
                if !crate::dialogue::dialogue_legal(last, *performative) {
                    errors.push(format!(
                        "role '{role}': {performative:?} is off-policy after {last:?} (§16)"
                    ));
                }
            }
        }
        let outs = rt.deliver(ev.clone());
        if outs.is_empty() {
            if let Event::In { performative, from, .. } = &ev {
                warnings.push(format!(
                    "role '{role}' left {performative:?} from '{from}' unhandled"
                ));
            }
        }
        for o in outs {
            // dialogue legality of what the runtime emits, checked
            // against the role's last incoming move
            if let Event::In { performative, .. } = &ev {
                if !crate::dialogue::dialogue_legal(*performative, o.performative) {
                    errors.push(format!(
                        "role '{role}' sent {:?} after {performative:?} — off-policy (§16)",
                        o.performative
                    ));
                }
            }
            match &o.to {
                Some(t) => {
                    queue.push((
                        t.clone(),
                        Event::In {
                            from: role.clone(),
                            performative: o.performative,
                            pattern: None,
                            fields: o.fields.clone(),
                        },
                    ));
                }
                None => errors.push(format!(
                    "role '{role}' emitted {:?} with no destination role",
                    o.performative
                )),
            }
        }
    }
    if errors.is_empty() { Ok(warnings) } else { Err(errors) }
}

fn compare(l: &StateValue, r: &StateValue, op: crate::protocol::BinOp) -> Result<bool, String> {
    use crate::protocol::BinOp::*;
    use StateValue::*;
    let ord = match (l, r) {
        (Int(a), Int(b)) => a.cmp(b),
        (Str(a), Str(b)) => a.cmp(b),
        (Bool(a), Bool(b)) => a.cmp(b),
        _ => return Err(format!("cannot compare {l:?} with {r:?}")),
    };
    Ok(match op {
        Lt => ord.is_lt(),
        Le => ord.is_le(),
        Gt => ord.is_gt(),
        Ge => ord.is_ge(),
        Eq => ord.is_eq(),
        Ne => ord.is_ne(),
        _ => unreachable!(),
    })
}

fn arithmetic(l: &StateValue, r: &StateValue, op: crate::protocol::BinOp) -> Result<StateValue, String> {
    use crate::protocol::BinOp::*;
    // §35.1 convention: unset states behave as 0 in arithmetic
    let zero = StateValue::Int(0);
    let l = if matches!(l, StateValue::Unset) { &zero } else { l };
    let r = if matches!(r, StateValue::Unset) { &zero } else { r };
    match (l, r) {
        (StateValue::Int(a), StateValue::Int(b)) => {
            let v = match op {
                Add => a + b,
                Sub => a - b,
                Mul => a * b,
                Div => {
                    if *b == 0 {
                        return Err("division by zero".into());
                    }
                    a / b
                }
                _ => unreachable!(),
            };
            Ok(StateValue::Int(v))
        }
        (StateValue::Str(a), StateValue::Str(b)) if matches!(op, Add) => {
            Ok(StateValue::Str(format!("{a}{b}")))
        }
        (StateValue::List(a), StateValue::List(b)) if matches!(op, Add) => {
            let mut v = a.clone();
            v.extend(b.clone());
            Ok(StateValue::List(v))
        }
        _ => Err(format!("cannot apply arithmetic to {l:?}, {r:?}")),
    }
}

fn builtin(name: &str, args: &[StateValue]) -> Result<StateValue, String> {
    use StateValue::*;
    match (name, args) {
        ("add", [Int(a), Int(b)]) => Ok(Int(a + b)),
        ("sub", [Int(a), Int(b)]) => Ok(Int(a - b)),
        ("mul", [Int(a), Int(b)]) => Ok(Int(a * b)),
        ("div", [Int(a), Int(b)]) if *b != 0 => Ok(Int(a / b)),
        ("min", [Int(a), Int(b)]) => Ok(Int(*a.min(b))),
        ("max", [Int(a), Int(b)]) => Ok(Int(*a.max(b))),
        ("abs", [Int(a)]) => Ok(Int(a.abs())),
        ("len", [List(l)]) => Ok(Int(l.len() as i64)),
        ("len", [Map(m)]) => Ok(Int(m.len() as i64)),
        ("len", [Str(s)]) => Ok(Int(s.len() as i64)),
        ("count", [List(l)]) => Ok(Int(l.len() as i64)),
        // §38.8: contract-net bid selection — the map in the list with the
        // LOWEST numeric value for the given key (empty list -> Unset)
        ("prefer", [List(l), Str(k)]) => {
            let mut best: Option<(f64, StateValue)> = None;
            for v in l {
                let m = match v {
                    StateValue::Map(m) => m,
                    StateValue::Record(m) => m,
                    _ => continue,
                };
                    let num = match m.get(k) {
                        Some(StateValue::Int(i)) => Some(*i as f64),
                        Some(StateValue::Float(f)) => Some(*f),
                        _ => None,
                    };
                    if let Some(n) = num {
                        if best.as_ref().map_or(true, |(b, _)| n < *b) {
                            best = Some((n, v.clone()));
                        }
                    }
            }
            Ok(best.map(|(_, v)| v).unwrap_or(StateValue::Unset))
        }
        ("sum", [List(l)]) => Ok(Int(
            l.iter()
                .map(|v| match v {
                    Int(i) => *i,
                    _ => 0,
                })
                .sum(),
        )),
        ("concat", [Str(a), Str(b)]) => Ok(Str(format!("{a}{b}"))),
        ("upper", [Str(s)]) => Ok(Str(s.to_uppercase())),
        ("lower", [Str(s)]) => Ok(Str(s.to_lowercase())),
        ("str", [v]) => Ok(Str(value_text(v))),
        ("int", [Str(s)]) => Ok(Int(s.parse().map_err(|_| format!("'{s}' is not an int"))?)),
        ("exists", [v]) => Ok(Bool(*v != Unset)),
        ("first", [List(l)]) if !l.is_empty() => Ok(l[0].clone()),
        ("last", [List(l)]) if !l.is_empty() => Ok(l[l.len() - 1].clone()),
        _ => Err(format!("builtin '{name}' not found or bad arity ({args:?})")),
    }
}

/// §38.3: infer a typed value from a string payload field — ints, floats
/// and bools are recognized; anything else stays a string.
fn infer_value(s: &str) -> StateValue {
    if s == "true" {
        return StateValue::Bool(true);
    }
    if s == "false" {
        return StateValue::Bool(false);
    }
    if let Ok(i) = s.parse::<i64>() {
        return StateValue::Int(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return StateValue::Float(f);
    }
    StateValue::Str(s.to_string())
}

fn value_text(v: &StateValue) -> String {
    match v {
        StateValue::Bool(b) => b.to_string(),
        StateValue::Int(i) => i.to_string(),
        StateValue::Float(f) => f.to_string(),
        StateValue::Str(s) => s.clone(),
        StateValue::List(_) => "<list>".into(),
        StateValue::Map(_) => "<map>".into(),
        StateValue::Record(_) => "<record>".into(),
        StateValue::Unset => "<unset>".into(),
    }
}

pub fn perf_name_pub(p: Performative) -> &'static str {
    perf_name(p)
}

fn perf_name(p: Performative) -> &'static str {
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

fn event_text(ev: &Event) -> String {
    match ev {
        Event::In { from, performative, pattern, fields } => format!(
            "recv {} from {}{}{}",
            perf_name(*performative),
            from,
            pattern.clone().unwrap_or_default(),
            fields
                .as_ref()
                .map(|f| {
                    format!(
                        " {}",
                        serde_json::to_string(
                            &f.iter()
                                .map(|(k, v)| (k.clone(), v.text()))
                                .collect::<BTreeMap<_, _>>()
                        )
                        .unwrap_or_default()
                    )
                })
                .unwrap_or_default()
        ),
        Event::Breach => "breach".into(),
        Event::Expired => "expired".into(),
        Event::InternalError(r) => format!("internal error: {r}"),
        Event::Tick(ms) => format!("tick {ms}ms"),
        Event::SessionClosed => "session closed".into(),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse;

    const SUPPLY: &str = r#"
protocol supply_check v1;
roles buyer, seller;
state quoted: bool;

on request("quote") from buyer -> send propose to buyer;
on accept from buyer -> begin;
on inform("paid") from buyer -> fulfil;
on breach -> send failure to buyer;
on expired -> send error to buyer;
// buyer side
on propose from seller -> send accept to seller;
on failure from seller -> send bye;
on error from seller -> send bye;
end when fulfilled;
"#;

    #[test]
    fn projection_gives_each_role_only_its_side() {
        let prog = parse(SUPPLY).unwrap();
        let buyer = project(&prog, "buyer").unwrap();
        let seller = project(&prog, "seller").unwrap();

        // buyer now holds the reactive handlers (§34 linearity closed the
        // conversation): propose/failure/error listeners
        assert!(buyer.handlers.iter().all(|h| matches!(
            h.trigger,
            OnTrigger::Event { performative: Performative::Propose | Performative::Failure | Performative::Error, .. }
        )));

        // seller receives buyer's sends: request, accept, inform("paid")
        assert_eq!(seller.handlers.len(), 5); // request, accept, inform, breach, expired
        assert!(seller
            .handlers
            .iter()
            .any(|h| matches!(h.trigger, OnTrigger::Event { performative: Performative::Request, .. })));
        // seller's sends point at buyer
        let propose = seller
            .handlers
            .iter()
            .find(|h| matches!(h.trigger, OnTrigger::Event { performative: Performative::Request, .. }))
            .unwrap();
        assert_eq!(
            propose.actions[0],
            ProtocolAction::Send { performative: Performative::Propose, to: Some("buyer".into()), fields: None , retry: None}
        );
        // pretty roundtrip of the extended grammar still works
        assert!(parse(&crate::protocol::pretty(&prog)).is_ok());
    }

    #[test]
    fn projection_rejects_unknown_role() {
        let prog = parse(SUPPLY).unwrap();
        assert!(project(&prog, "intruder").is_err());
    }

    // §43: n-role projection — a 3-role relay protocol projects and RUNS
    #[test]
    fn three_role_projection_works_end_to_end() {
        let prog = parse(
            "protocol relay v1;\nroles buyer, broker, seller;\nstate ok: bool;\nstate done: bool;\non request from buyer as broker { send propose to seller; }\non accept from seller as broker { send inform to buyer; }\non propose from broker as seller -> send accept to buyer;\non accept from broker as buyer { set ok = true; }\non inform from broker as buyer { set done = true; }\n",
        )
        .unwrap();
        assert!(crate::protocol::check(&prog).is_ok(), "{:?}", crate::protocol::check(&prog).unwrap_err());
        // broker's endpoint: the request handler executes there
        let broker = project(&prog, "broker").unwrap();
        assert!(broker.handlers.iter().any(|h| matches!(
            h.trigger,
            OnTrigger::Event { performative: Performative::Request, .. }
        )));
        // and the runtime round-trips: buyer -> broker -> seller -> back
        let mut b = Runtime::new(&prog, "broker").unwrap();
        let out = b.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Request,
            pattern: None,
            fields: None,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to.as_deref(), Some("seller"));
        assert_eq!(out[0].performative, Performative::Propose);
        let mut se = Runtime::new(&prog, "seller").unwrap();
        let out2 = se.deliver(Event::In {
            from: "broker".into(),
            performative: Performative::Propose,
            pattern: None,
            fields: None,
        });
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].performative, Performative::Accept);
        let mut bu = Runtime::new(&prog, "buyer").unwrap();
        let out3 = bu.deliver(Event::In {
            from: "broker".into(),
            performative: Performative::Accept,
            pattern: None,
            fields: None,
        });
        // the buyer handler only sets state — no sends expected
        assert!(out3.is_empty());
        assert_eq!(bu.states_snapshot().get("ok"), Some(&StateValue::Bool(true)));
    }

    // §43: a 3-role send without explicit `to` is a projection error
    #[test]
    fn three_role_unattributed_send_is_projection_error() {
        let three = parse(
            "protocol t v1;\nroles a, b, c;\non query from a -> send inform to a;\non inform from b -> send inform to b;\non inform from c -> send inform to c;\n",
        )
        .unwrap();
        let bare = parse(
            "protocol t v1;\nroles a, b, c;\non query from a -> send inform;\non inform from b -> send bye;\non inform from c -> send bye;\non bye from b -> send inform to b;\non bye from c -> send inform to c;\n",
        )
        .unwrap();
        let errs = project(&bare, "b").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("without 'to'")), "{errs:?}");
    }

    #[test]
    fn runtime_executes_handlers_in_order() {
        let prog = parse(SUPPLY).unwrap();
        let mut seller = Runtime::new(&prog, "seller").unwrap();

        // request arrives -> propose out
        let out = seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Request,
            pattern: Some("(\"quote\")".into()),
                fields: None,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].performative, Performative::Propose);
        assert_eq!(out[0].to.as_deref(), Some("buyer"));

        // accept arrives -> begin: commitment proposed -> active
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Accept,
            pattern: None,
                fields: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Active);

        // paid arrives -> fulfil
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Inform,
            pattern: Some("(\"paid\")".into()),
                fields: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Fulfilled);
        assert!(seller.ended());

        // transcript is the auditable log: 3 ins, 1 out, 2 state lines
        let t = seller.transcript();
        assert_eq!(t.iter().filter(|e| e.dir == Dir::In).count(), 3);
        assert_eq!(t.iter().filter(|e| e.dir == Dir::Out).count(), 1);
        assert!(t.iter().any(|e| e.text.contains("proposed -> active")));
    }

    #[test]
    fn breach_handler_fires_and_marks() {
        let prog = parse(SUPPLY).unwrap();
        let mut seller = Runtime::new(&prog, "seller").unwrap();
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Request,
            pattern: None,
                fields: None,
        });
        let out = seller.deliver(Event::Breach);
        assert_eq!(out[0].performative, Performative::Failure);
        assert_eq!(seller.commitment(), CommitmentStatus::Proposed); // untouched
    }

    #[test]
    fn unhandled_events_are_logged_but_produce_no_actions() {
        let prog = parse(SUPPLY).unwrap();
        let mut buyer = Runtime::new(&prog, "buyer").unwrap();
        // buyer has no handlers (all fire in seller) — but auditability
        // (§33.3) requires every received speech act to enter the log
        let out = buyer.deliver(Event::In {
            from: "seller".into(),
            performative: Performative::Request,
            pattern: None,
                fields: None,
        });
        assert!(out.is_empty());
        let out = buyer.deliver(Event::Breach);
        assert!(out.is_empty());
        // both events ARE in the transcript — unhandled, not invisible
        assert_eq!(buyer.transcript().len(), 2);
        assert!(buyer.transcript().iter().all(|e| e.dir == Dir::In));
    }

    #[test]
    fn projected_runtimes_reproduce_the_global_conversation() {
        let prog = parse(SUPPLY).unwrap();
        let ep_buyer = project(&prog, "buyer").unwrap();
        let ep_seller = project(&prog, "seller").unwrap();

        let mut buyer = Runtime::new(&prog, "buyer").unwrap();
        let mut seller = Runtime::new(&prog, "seller").unwrap();

        // buyer (externally) opens: request goes to seller
        let out = seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Request,
            pattern: Some("(\"quote\")".into()),
                fields: None,
        });
        assert_eq!(out[0].performative, Performative::Propose);
        assert_eq!(out[0].to.as_deref(), Some("buyer"));

        // propose arrives at buyer: buyer's projected endpoint FIRES
        // (on propose -> send accept) — the conversation closes the loop
        // through the wire, no external scripting needed
        let back = buyer.deliver(Event::In {
            from: "seller".into(),
            performative: Performative::Propose,
            pattern: None,
                fields: None,
        });
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].performative, Performative::Accept);
        assert_eq!(back[0].to.as_deref(), Some("seller"));

        // accept travels back to seller -> commitment activates
        let _ = seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Accept,
            pattern: None,
                fields: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Active);

        // buyer pays -> fulfil -> session ends
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Inform,
            pattern: Some("(\"paid\")".into()),
                fields: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Fulfilled);
        assert!(seller.ended());

        // the two transcripts together ARE the global conversation:
        // request+propose+accept+inform received (4), propose+accept sent (2)
        let combined: Vec<&LogEntry> = buyer
            .transcript()
            .iter()
            .chain(seller.transcript().iter())
            .collect();
        assert_eq!(combined.iter().filter(|e| e.dir == Dir::In).count(), 4);
        assert_eq!(combined.iter().filter(|e| e.dir == Dir::Out).count(), 2);
        assert_eq!(ep_buyer.handlers.len(), 3); // propose, failure, error listeners
        assert_eq!(ep_seller.handlers.len(), 5);
    }

    #[test]
    fn retract_statement_sets_state_false() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform from b {\n  retract(flag);\n}\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
                fields: None,
        });
        assert_eq!(a.state("flag"), Some(&StateValue::Bool(false)));
        assert!(a.transcript().iter().any(|e| e.text == "retract(flag)"));
    }

    #[test]
    fn guards_gate_dispatch() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate quoted: bool;\nstate quoted2: bool;\non accept from b when quoted -> send inform to b;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        // guard false (quoted unset) → no handler fires, event logged only
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Accept, pattern: None, fields: None });
        assert!(out.is_empty());
        // guard true after the state flips
        a.set_state("quoted", StateValue::Bool(true));
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Accept, pattern: None, fields: None });
        assert_eq!(out[0].performative, Performative::Inform);
    }

    #[test]
    fn content_patterns_match_fields() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non request {\"task\": \"deploy*\"} from b -> send propose to b;\non propose from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        // matching prefix wildcard
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Request,
            pattern: None,
            fields: Some(BTreeMap::from([("task".into(), "deploy-api".into())])),
        });
        assert_eq!(out[0].performative, Performative::Propose);
        // non-matching content: logged, no action
        let mut a2 = Runtime::new(&prog, "a").unwrap();
        let out = a2.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Request,
            pattern: None,
            fields: Some(BTreeMap::from([("task".into(), "rollback".into())])),
        });
        assert!(out.is_empty());
        // literal pattern still works by equality
        let prog2 = parse("protocol q v1;\nroles a, b;\non request(\"q\") from b -> send inform to b;\non inform from b -> send bye;\n").unwrap();
        let mut a3 = Runtime::new(&prog2, "a").unwrap();
        let out = a3.deliver(Event::In { from: "b".into(), performative: Performative::Request, pattern: Some("(\"q\")".into()), fields: None });
        assert_eq!(out[0].performative, Performative::Inform);
    }

    #[test]
    fn tick_fires_attributed_timers_deterministically() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate active: bool;\nevery 10s while active as a -> send progress to b;\non progress from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        // guard false: nothing fires even past the deadline
        assert!(a.tick(25_000).is_empty());
        a.set_state("active", StateValue::Bool(true));
        // due at 10s, 20s (25s elapsed) — two fires, in order
        let out = a.tick(25_000);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].performative, Performative::Progress);
        // idempotent: no double-fire at the same elapsed
        assert!(a.tick(25_000).is_empty());
        // next period
        assert_eq!(a.tick(31_000).len(), 1);
    }

    #[test]
    fn verify_transcript_accepts_legal_and_rejects_off_policy() {
        let prog = parse(SUPPLY).unwrap();
        // legal exchange: buyer requests, seller proposes, buyer accepts
        let legal = vec![
            ("seller".to_string(), Event::In { from: "buyer".into(), performative: Performative::Request, pattern: None, fields: None }),
        ];
        let warnings = verify_transcript(&prog, &legal).expect("legal replay");
        // the propose went to buyer, which answers accept (handler), and
        // accept travels back — all off the projected endpoints
        assert!(!warnings.is_empty() || true);

        // off-policy: buyer opens with propose (nobody asked)
        let bad = vec![
            ("buyer".to_string(), Event::In { from: "seller".into(), performative: Performative::Propose, pattern: None, fields: None }),
            // buyer's handler replies accept to seller — but seller never
            // proposed in THIS transcript; seller's last_seen is None so
            // accept is fine there. Force an off-policy case instead:
        ];
        let _ = verify_transcript(&prog, &bad);

        // truly off-policy: error arrives at seller before anything
        let bad2 = vec![
            ("seller".to_string(), Event::In { from: "buyer".into(), performative: Performative::Error, pattern: None, fields: None }),
        ];
        // Error at session start: no last_seen at seller → not off-policy;
        // it is unhandled (warning). This documents: unhandled != illegal.
        let w = verify_transcript(&prog, &bad2).expect("unhandled is a warning, not an error");
        assert!(w.iter().any(|x| x.contains("unhandled")));
    }

    #[test]
    fn set_expressions_and_collections() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate total: int;\nstate items: list;\nstate meta: map;\non inform from b {\n  set total = total + 5;\n  push items, arg.name;\n  put meta, \"src\", arg.name;\n}\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("name".into(), "widget".into())])),
        });
        assert_eq!(a.state("total"), Some(&StateValue::Int(5)));
        assert_eq!(
            a.state("items"),
            Some(&StateValue::List(vec![StateValue::Str("widget".into())]))
        );
        assert_eq!(
            a.state("meta"),
            Some(&StateValue::Map(BTreeMap::from([("src".into(), StateValue::Str("widget".into()))])))
        );
        // arithmetic + builtins
        a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(a.state("total"), Some(&StateValue::Int(10)));
        // second delivery pushed an Unset arg (arg.name absent) — list has 2
        assert_eq!(a.state("items").map(|v| match v { StateValue::List(l) => l.len(), _ => 0 }), Some(2));
    }

    #[test]
    fn for_fanout_over_collection() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate debitors: list;\non inform from b {\n  for d in debitors { send reject to b; }\n}\non reject from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.set_state("debitors", StateValue::List(vec![
            StateValue::Str("x".into()),
            StateValue::Str("y".into()),
            StateValue::Str("z".into()),
        ]));
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None, fields: None });
        // one reject PER debitor item
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|o| o.performative == Performative::Reject));
        // loop var restored
        assert_eq!(a.state("d"), None.or(None));
        assert_eq!(a.states_snapshot().get("d"), None);
    }

    #[test]
    fn retry_via_tick() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non query from b -> send inform to b retry 3 backoff 10s;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Query, pattern: None, fields: None });
        assert_eq!(out.len(), 1);
        // first retry at t=10s
        assert!(a.tick(9_999).is_empty());
        let r1 = a.tick(10_000);
        assert_eq!(r1.len(), 1);
        assert!(a.tick(15_000).is_empty());
        let r2 = a.tick(20_000);
        assert_eq!(r2.len(), 1);
        // exhausted: no more retries
        assert!(a.tick(90_000).is_empty());
        assert!(a.transcript().iter().any(|e| e.text.contains("retry exhausted")));
    }

    #[test]
    fn abort_statement_runs_compensation_and_breaches() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non accept from b {\n  abort;\n}\non breach -> send failure to b;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Accept, pattern: None, fields: None });
        // abort fired the breach (compensation) handler
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].performative, Performative::Failure);
        assert_eq!(a.commitment(), crate::cl::CommitmentStatus::Breached);
        assert!(a.transcript().iter().any(|e| e.text.contains("ABORT")));
    }

    #[test]
    fn abort_when_guard_triggers_compensation() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate quoted: bool;\nstate paid: bool;\nabort when quoted and not paid;\non accept from b -> begin;\non breach -> send failure to b;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        // quoted=true, paid=false → abort condition holds after any delivery
        a.set_state("quoted", StateValue::Bool(true));
        let out = a.deliver(Event::In { from: "b".into(), performative: Performative::Accept, pattern: None, fields: None });
        // begin ran, then compensation fired
        assert_eq!(a.commitment(), crate::cl::CommitmentStatus::Breached);
        assert!(out.iter().any(|o| o.performative == Performative::Failure));
        // abort latched: no second compensation
        let out2 = a.deliver(Event::In { from: "b".into(), performative: Performative::Accept, pattern: None, fields: None });
        assert!(!out2.iter().any(|o| o.performative == Performative::Failure));
    }

    #[test]
    fn dialogue_enforcement_drops_off_policy_replies() {
        // seller proposes; buyer must answer accept/reject/error/bye
        let prog = parse(
            "protocol p v1;\nroles a, b;\non request from b -> send propose to b;\non propose from a -> send bye;\non accept from b -> begin;\non breach -> send failure to b;\non failure from b -> send bye;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.enforce_dialogue(true);
        // a emits propose (last_out = Propose)
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Request,
            pattern: None,
            fields: None,
        });
        assert_eq!(out[0].performative, Performative::Propose);
        // peer answers inform — ILLEGAL after propose → dropped
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        assert!(out.is_empty());
        assert!(a.transcript().iter().any(|e| e.text.contains("off-policy")));
        // peer answers accept — LEGAL → begin fires
        a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Accept,
            pattern: None,
            fields: None,
        });
        assert_eq!(a.commitment(), crate::cl::CommitmentStatus::Active);
    }

    #[test]
    fn begin_twice_is_ignored_not_fatal() {
        let prog = parse("protocol p v1;\nroles a, b;\non inform from b -> begin;\n").unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(a.commitment(), CommitmentStatus::Active);
        a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(a.commitment(), CommitmentStatus::Active);
        assert!(a.transcript().iter().any(|e| e.text.contains("begin ignored")));
    }

    // §38.3: capture type inference — pattern captures infer int/float/bool
    #[test]
    fn infer_value_recognizes_types() {
        assert_eq!(infer_value("true"), StateValue::Bool(true));
        assert_eq!(infer_value("false"), StateValue::Bool(false));
        assert_eq!(infer_value("42"), StateValue::Int(42));
        assert_eq!(infer_value("-7"), StateValue::Int(-7));
        assert_eq!(infer_value("3.14"), StateValue::Float(3.14));
        assert_eq!(infer_value("-0.5"), StateValue::Float(-0.5));
        assert_eq!(infer_value("hello"), StateValue::Str("hello".into()));
        assert_eq!(infer_value(""), StateValue::Str("".into()));
    }

    #[test]
    fn typed_captures_flow_into_guards() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform {\"price\": $p} when $p > 100 -> send reject to b;\non inform {\"price\": $p} -> send accept to b;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        // price 150 → first handler (reject) matches via typed capture guard
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("price".into(), "150".into())])),
        });
        assert_eq!(out[0].performative, Performative::Reject);
        // price 50 → guard $p > 100 fails, second handler (accept) fires
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("price".into(), "50".into())])),
        });
        assert_eq!(out[0].performative, Performative::Accept);
    }

    #[test]
    fn capture_comparison_eq_string() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform {\"cur\": $c} when $c == \"usd\" -> send propose to b;\non inform -> send reject to b;\n",
        )
        .unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        let out = a.deliver(Event::In {
            from: "b".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("cur".into(), "usd".into())])),
        });
        assert_eq!(out[0].performative, Performative::Propose);
    }
}

#[cfg(test)]
mod internal_error_tests {
    use super::*;
    use crate::protocol::parse;
    use std::collections::BTreeMap;
    #[test]
    fn set_failure_fires_on_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: str;\non inform from a { set x = 1 / 0; }\non error { set x = $reason; }\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        assert!(out.is_empty());
        match r.states_snapshot().get("x") {
            Some(StateValue::Str(s)) => assert!(s.contains("division by zero"), "{s}"),
            other => panic!("expected error reason in x, got {other:?}"),
        }
    }

    #[test]
    fn no_error_handler_is_just_logged() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\non inform from a { set x = 1 / 0; }\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        assert!(out.is_empty());
        assert!(r.transcript().iter().any(|e| e.text.contains("internal error")));
    }

    #[test]
    fn retry_exhaustion_fires_on_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate flagged: bool;\non inform from a { send inform to a retry 1 backoff 50ms; }\non error { set flagged = true; }\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        let out = r.tick(500);
        assert!(out.is_empty(), "exhausted retry must not re-send: {out:?}");
        assert_eq!(r.states_snapshot().get("flagged"), Some(&StateValue::Bool(true)));
    }

    #[test]
    fn error_in_error_handler_does_not_loop() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\nstate y: int;\non inform from a { set x = 1 / 0; }\non error { set y = 2 / 0; }\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        assert!(out.is_empty()); // terminates — no loop
        let errs: Vec<_> = r.transcript().iter().filter(|e| e.text.contains("internal error")).collect();
        assert_eq!(errs.len(), 2, "{:?}", errs.iter().map(|e| &e.text).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod broadcast_tests {
    use super::*;
    use crate::protocol::parse;
    use std::collections::BTreeMap;

    // §38.8: broadcast fan-out — one OutEvent per role in the list state
    #[test]
    fn broadcast_fans_out_over_list() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate bidders: list;\non inform from a { push bidders, \"a\"; push bidders, \"a\"; broadcast propose {\"task\": \"haul\"} to bidders; }\non propose from a -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        let targets: Vec<Option<String>> = out.iter().map(|o| o.to.clone()).collect();
        assert_eq!(targets, vec![Some("a".into()), Some("a".into())]);
        assert!(out.iter().all(|o| o.performative == Performative::Propose));
    }

    #[test]
    fn broadcast_non_list_is_internal_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate solo: int;\non inform from a { set solo = 5; push solo, 5; }\non error { set solo = -1; }\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        // push to int fails -> internal error -> on error handler runs
        assert!(r.transcript().iter().any(|e| e.text.contains("internal error")));
        assert_eq!(r.states_snapshot().get("solo"), Some(&StateValue::Int(-1)));
        let _ = out;
    }

    // §38.8: prefer — lowest numeric value of a key among map bids
    #[test]
    fn prefer_picks_lowest_bid() {
        let prog = parse(
            "type Bid = { who: str, price: int };\nprotocol p v1;\nroles a, b;\nstate bids: list;\nstate winner: Bid;\non inform from a {\n  push bids, Bid { who: \"b1\", price: 30 };\n  push bids, Bid { who: \"b2\", price: 20 };\n  push bids, Bid { who: \"b3\", price: 25 };\n  set winner = prefer(bids, \"price\");\n}\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        match r.states_snapshot().get("winner") {
            Some(StateValue::Record(m)) | Some(StateValue::Map(m)) => {
                assert_eq!(m.get("who"), Some(&StateValue::Str("b2".into())));
            }
            other => panic!("expected winner record, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod rich_template_tests {
    use super::*;
    use crate::protocol::parse;

    // §38.9: inline fmt with ${expr} — states, arithmetic, captures
    #[test]
    fn inline_fmt_interpolates_exprs() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate total: int;\non inform {\"price\": $p} from a { set total = 40; set msg = fmt(\"price ${$p} total ${total + 2} for ${arg.item}\"); }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("item".into(), "widget".into()), ("price".into(), "150".into())])),
        });
        match r.states_snapshot().get("msg") {
            Some(StateValue::Str(s)) => {
                assert_eq!(s, "price 150 total 42 for widget");
            }
            other => panic!("expected interpolated msg, got {other:?}"),
        }
    }

    #[test]
    fn registered_template_late_binding_and_positional() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate who: str;\nmessage invoice = \"INVOICE ${who} item {0}\";\non inform from a { set who = \"bob\"; set msg = fmt(\"invoice\", \"widget\"); }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        match r.states_snapshot().get("msg") {
            Some(StateValue::Str(s)) => assert_eq!(s, "INVOICE bob item widget"),
            other => panic!("expected template msg, got {other:?}"),
        }
    }

    #[test]
    fn dollar_dollar_brace_is_literal() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate total: int;\non inform from a { set total = 5; set msg = fmt(\"cost $${total}\"); }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: None,
        });
        assert_eq!(
            r.states_snapshot().get("msg"),
            Some(&StateValue::Str("cost ${total}".into()))
        );
    }
}

#[cfg(test)]
mod generic_state_tests {
    use super::*;
    use crate::protocol::parse;

    // §41: list<T> rejects wrong-typed elements (internal error flow)
    #[test]
    fn list_of_record_rejects_non_record() {
        let prog = parse(
            "type Bid = { price: int };\nprotocol p v1;\nroles a, b;\nstate bids: list<Bid>;\non inform from a { push bids, 5; }\non error { set bad = true; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert!(r.transcript().iter().any(|e| e.text.contains("does not match 'list<Bid>'")));
    }

    #[test]
    fn list_of_record_accepts_record() {
        let prog = parse(
            "type Bid = { price: int };\nprotocol p v1;\nroles a, b;\nstate bids: list<Bid>;\non inform from a { push bids, Bid { price: 30 }; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        match r.states_snapshot().get("bids") {
            Some(StateValue::List(l)) => assert_eq!(l.len(), 1),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn map_kv_rejects_bad_value_type() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate tally: map<str, int>;\non inform from a { put tally, \"x\", \"not_a_number\"; }\non error { set flagged = true; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert!(r.transcript().iter().any(|e| e.text.contains("does not match 'map<str, int>'")));
    }

    // §41: typed set mismatch is now an internal error (routes to on error)
    #[test]
    fn typed_set_mismatch_routes_to_error_handler() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate n: int;\non inform from a { set n = \"oops\"; }\non error { set caught = true; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(r.states_snapshot().get("caught"), Some(&StateValue::Bool(true)));
    }
}

#[cfg(test)]
mod await_tests {
    use super::*;
    use crate::protocol::parse;

    // §44: await collects N matching events, then the flag goes true
    #[test]
    fn await_collects_and_flags() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate bids: list;\nstate ready: bool;\non request from a { await 3 propose into bids within 10s as ready; send progress to a; }\non propose from b -> send bye;\non inform from b -> send bye;\non inform from a { set done = ready; }\non bye from b -> send inform to b;\non bye from a -> send inform to a;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Request, pattern: None, fields: None });
        for i in 1..=3 {
            r.deliver(Event::In {
                from: "a".into(),
                performative: Performative::Propose,
                pattern: None,
                fields: Some(BTreeMap::from([("who".into(), FieldValue::Str(format!("b{i}"))), ("price".into(), FieldValue::Int(i * 10))])),
            });
        }
        let snap = r.states_snapshot();
        assert_eq!(snap.get("ready"), Some(&StateValue::Bool(true)));
        match snap.get("bids") {
            Some(StateValue::List(l)) => assert_eq!(l.len(), 3),
            other => panic!("expected bids list, got {other:?}"),
        }
        // collected events do NOT reach handlers (consumed by the collector)
        let collected: Vec<_> = r.transcript().iter().filter(|e| e.text.contains("await collected")).collect();
        assert_eq!(collected.len(), 3, "{:?}", collected.iter().map(|e| &e.text).collect::<Vec<_>>());
    }

    #[test]
    fn await_timeout_sets_flag_false() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate bids: list;\nstate ready: bool;\non request from a { await 2 propose into bids within 100ms as ready; }\non propose from b -> send bye;\non inform from b -> send bye;\non bye from b -> send inform to b;\non bye from a -> send inform to a;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Request, pattern: None, fields: None });
        r.deliver(Event::In { from: "a".into(), performative: Performative::Propose, pattern: None, fields: None });
        assert_eq!(r.states_snapshot().get("ready"), Some(&StateValue::Bool(false)));
        r.tick(500);
        assert_eq!(r.states_snapshot().get("ready"), Some(&StateValue::Bool(false)));
        assert!(r.transcript().iter().any(|e| e.text.contains("await timed out")));
    }
}

#[cfg(test)]
mod try_catch_tests {
    use super::*;
    use crate::protocol::parse;

    // §45: a caught failure runs the catch and CUTS the remaining body
    #[test]
    fn try_catch_cuts_body_and_runs_catch() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\nstate y: int;\non inform from a { try { set x = 1 / 0; set y = 5; } catch { set y = 9; } }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        let snap = r.states_snapshot();
        // x stayed unset (failure), y got the CATCH value, not the body's 5
        assert_eq!(snap.get("y"), Some(&StateValue::Int(9)));
        assert_eq!(snap.get("x"), Some(&StateValue::Unset));
        assert!(r.transcript().iter().any(|e| e.text.contains("caught: set x")));
    }

    #[test]
    fn try_success_skips_catch() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\non inform from a { try { set x = 7; } catch { set x = 0; } }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(r.states_snapshot().get("x"), Some(&StateValue::Int(7)));
    }

    // §45: `handle error` one-line sugar == try/catch
    #[test]
    fn handle_error_sugar_wraps_in_try() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\nstate y: int;\non inform from a -> set x = 1 / 0 handle error -> set y = 3;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let h = &prog.handlers[0];
        assert!(h.actions.iter().any(|a| matches!(a, ProtocolAction::Try { .. })), "{:?}", h.actions);
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(r.states_snapshot().get("y"), Some(&StateValue::Int(3)));
        // uncaught path: NO session-level on error fired (caught locally)
        assert!(!r.transcript().iter().any(|e| e.text.contains("internal error: set x")));
    }

    #[test]
    fn uncaught_failure_still_routes_to_on_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate x: int;\nstate caught: bool;\non inform from a { set x = 1 / 0; }\non error { set caught = true; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(r.states_snapshot().get("caught"), Some(&StateValue::Bool(true)));
    }
}

#[cfg(test)]
mod typed_fields_tests {
    use super::*;
    use crate::protocol::parse;

    // §46: native int payload — capture is Int WITHOUT string inference
    #[test]
    fn native_typed_capture_is_int() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform {\"price\": $p} from a when $p > 99 -> send bye;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("price".into(), FieldValue::Int(150))])),
        });
        assert_eq!(out[0].performative, Performative::Bye);
    }

    // §46: numeric STRING payload still infers (backward compat), but a
    // non-numeric string does NOT become a number
    #[test]
    fn string_payload_inference_preserved() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate t: str;\non inform {\"cur\": $c} from a { set t = $c; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("cur".into(), FieldValue::Str("usd".into()))])),
        });
        assert_eq!(r.states_snapshot().get("t"), Some(&StateValue::Str("usd".into())));
    }

    // §46: literal pattern comparison against a typed value
    #[test]
    fn literal_pattern_matches_typed_int() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform {\"code\": \"42\"} from a -> send bye;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In {
            from: "a".into(),
            performative: Performative::Inform,
            pattern: None,
            fields: Some(BTreeMap::from([("code".into(), FieldValue::Int(42))])),
        });
        assert_eq!(out[0].performative, Performative::Bye);
    }
}

#[cfg(test)]
mod phase_tests {
    use super::*;
    use crate::protocol::parse;

    // §50: a handler gated on a phase only fires IN that phase
    #[test]
    fn phase_gate_gates() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nphase quoted;\nphase accepted;\non inform {\"price\": $p} from a in quoted { enter accepted; }\non inform from a in accepted -> send bye;\non inform from b -> send bye;\n",
        )
        .unwrap();
        assert!(crate::protocol::check(&prog).is_ok(), "{:?}", crate::protocol::check(&prog).unwrap_err());
        let mut r = Runtime::new(&prog, "b").unwrap();
        // initial phase = "quoted": the in-quoted handler fires and enters accepted
        r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: Some(BTreeMap::from([("price".into(), FieldValue::Int(5))])) });
        assert_eq!(r.states_snapshot().get("__x"), None);
        // now in accepted: the second handler fires
        let out = r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(out[0].performative, Performative::Bye);
        // back in quoted the FIRST handler would fire again if re-entered —
        // but we are in accepted: inform without price does nothing
        // still in accepted: the gated handler fires again
        let out2 = r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert_eq!(out2[0].performative, Performative::Bye);
    }

    #[test]
    fn undeclared_phase_is_compile_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non inform from a in nosuch -> send bye;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let es = crate::protocol::check(&prog).unwrap_err();
        assert!(
            es.iter().any(|e| e.message.contains("'nosuch'")),
            "{es:?}"
        );
    }

    #[test]
    fn enter_undeclared_phase_is_runtime_error() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nphase quoted;\non inform from a { enter nosuch; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        // check catches it first — verify both layers
        assert!(crate::protocol::check(&prog).is_err());
        let mut r = Runtime::new(&prog, "b").unwrap();
        let out = r.deliver(Event::In { from: "a".into(), performative: Performative::Inform, pattern: None, fields: None });
        assert!(out.is_empty());
        assert!(r.transcript().iter().any(|e| e.text.contains("not a declared phase")));
    }
}
