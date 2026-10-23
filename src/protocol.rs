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
    Send { performative: Performative, to: Option<String>, fields: Option<BTreeMap<String, String>>, retry: Option<Retry> },
    /// `begin` — activate the pending commitment (proposed -> active)
    Begin,
    /// `fulfil` — mark the commitment fulfilled (active -> fulfilled)
    Fulfil,
    /// `retract(fact_name)` — belief revision as a statement (§32.1)
    Retract { target: String },
    /// §35.1: `set <state> = <expr>;` — `field` targets a record field
    /// (`set q.price = 5;`)
    Set { name: String, field: Option<String>, expr: Expr },
    /// §35.2: `push <state>, <expr>;` — append to a list state
    Push { state: String, expr: Expr },
    /// §35.2: `put <state>, <key>, <expr>;` — set a map entry
    Put { state: String, key: String, expr: Expr },
    /// §35.3: `for <var> in <list_state> { ... }` — fan-out over a collection
    For { var: String, over: String, actions: Vec<ProtocolAction> },
    /// §35.4: `abort;` — trigger compensation handlers immediately
    Abort,
    /// §38.1: `return <expr>;` — only legal inside a function body
    Return { expr: Expr },
}

/// §35.5: retry policy on sends — declarative, driven by the runtime's
/// own deterministic timers (no clock).
#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub attempts: u32,
    pub backoff_ms: i64,
    /// exponential (double) backoff between attempts
    pub exponential: bool,
}

/// §34: guard — boolean expression over session states, evaluated at
/// dispatch time. `quoted`, `not quoted`, `a and b`, `a or b`.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    Atom { name: String, neg: bool },
    And(Box<Guard>, Box<Guard>),
    Or(Box<Guard>, Box<Guard>),
    /// §38.3: full expression guard — `when $p > 100` — evaluated with
    /// the trigger's captures in scope (captures shadow session states)
    ExprGuard(Expr),
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
            Guard::ExprGuard(_) => true, // resolved by the runtime (needs captures)
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
            Guard::ExprGuard(_) => vec![], // runtime resolves captures
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
    // §38.3: expression guards — `when $p > 100`, `when $p == "x"` — any
    // guard containing a capture reference or a comparison operator is an
    // expression guard (parsed and evaluated by the runtime with captures)
    if src.contains('$') || src.contains('<') || src.contains('>')
        || src.contains("==") || src.contains("!=") {
        return match Expr::parse_expr(src) {
            Ok(e) => Ok(Guard::ExprGuard(e)),
            Err(err) => Err(format!("bad expression guard '{src}': {err}")),
        };
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
/// §35: expression language for `set` — pure functions over session
/// state and the triggering event's fields. No I/O, no clock: the only
/// effects a program can cause are speech acts (§35.6).
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LitInt(i64),
    LitStr(String),
    LitBool(bool),
    State(String),
    /// §38.3: capture reference `$p` — resolves to the value captured by
    /// the trigger's pattern. Captures shadow session states.
    Capture(String),
    /// `arg.price` — field of the triggering event's structured payload
    Arg(String),
    Bin { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    Not(Box<Expr>),
    /// builtin pure functions: add sub mul div min max len sum count
    /// concat upper lower str int exists abs — plus user-defined
    /// §38.1 functions (resolved after builtins)
    Call { name: String, args: Vec<Expr> },
    /// §38.2: field access on records: `last_quote.price`
    Field { base: Box<Expr>, name: String },
    /// §38.2: record literal: `make(Quote, price: 5, currency: "usd")`
    /// (parsed via Call when the callee names a declared type)
    RecLit { ty: String, fields: Vec<(String, Expr)> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div,
    Lt, Le, Gt, Ge, Eq, Ne,
    And, Or,
}

impl Expr {
    /// parse one expression string (recursive descent, real tokenizer)
    pub fn parse_expr(src: &str) -> Result<Expr, String> {
        #[derive(Debug, Clone, PartialEq)]
        enum Tok {
            Word(String),
            Int(i64),
            Str(String),
            Op(&'static str),
        }
        fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
            let mut toks = Vec::new();
            let cs: Vec<char> = src.chars().collect();
            let mut i = 0;
            while i < cs.len() {
                let c = cs[i];
                if c.is_whitespace() { i += 1; continue; }
                if c == '"' {
                    let mut j = i + 1;
                    while j < cs.len() && cs[j] != '"' { j += 1; }
                    if j >= cs.len() { return Err("unterminated string".into()); }
                    toks.push(Tok::Str(cs[i + 1..j].iter().collect()));
                    i = j + 1;
                    continue;
                }
                if c.is_ascii_digit() {
                    let mut j = i;
                    while j < cs.len() && cs[j].is_ascii_digit() { j += 1; }
                    toks.push(Tok::Int(cs[i..j].iter().collect::<String>().parse().map_err(|_| "bad int")?));
                    i = j;
                    continue;
                }
                if c.is_alphabetic() || c == '_' || c == '$' {
                    let mut j = i;
                    // §38.3: '$' starts a capture token — consume it first,
                    // then the identifier chars (prevents zero-progress loop)
                    if cs[j] == '$' { j += 1; }
                    while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_' || cs[j] == '.') { j += 1; }
                    if j == i { return Err(format!("bad char '{c}' in expression")); }
                    toks.push(Tok::Word(cs[i..j].iter().collect()));
                    i = j;
                    continue;
                }
                if i + 1 < cs.len() {
                    let two: String = cs[i..i + 2].iter().collect();
                    let op = match two.as_str() {
                        "<=" => "<=", ">=" => ">=", "==" => "==", "!=" => "!=",
                        _ => "",
                    };
                    if !op.is_empty() {
                        toks.push(Tok::Op(match op { "<=" => "<=", ">=" => ">=", "==" => "==", _ => "!=" }));
                        i += 2;
                        continue;
                    }
                }
                let one = match c {
                    '+' => "+", '-' => "-", '*' => "*", '/' => "/",
                    '<' => "<", '>' => ">", '(' => "(", ')' => ")",
                    ',' => ",",
                    _ => return Err(format!("bad char '{c}' in expression")),
                };
                toks.push(Tok::Op(one));
                i += 1;
            }
            Ok(toks)
        }
        struct P {
            toks: Vec<Tok>,
            pos: usize,
        }
        impl P {
            fn peek(&self) -> Option<&Tok> { self.toks.get(self.pos) }
            fn next(&mut self) -> Option<Tok> { let t = self.peek().cloned(); self.pos += 1; t }
            fn atom(&mut self) -> Result<Expr, String> {
                match self.next() {
                    Some(Tok::Int(i)) => Ok(Expr::LitInt(i)),
                    Some(Tok::Str(s)) => Ok(Expr::LitStr(s)),
                    Some(Tok::Word(w)) => match w.as_str() {
                        "true" => Ok(Expr::LitBool(true)),
                        "false" => Ok(Expr::LitBool(false)),
                        "not" => Ok(Expr::Not(Box::new(self.atom_add()?))),
                        _ if w.starts_with('$') && w.len() > 1 => Ok(Expr::Capture(w[1..].to_string())),
                        _ if w.starts_with("arg.") => Ok(Expr::Arg(w[4..].to_string())),
                        _ if w.contains('.') && !w.starts_with('.') => {
                            // §38.2: field access chain `q.price` — the
                            // head segment is a state name, the rest are
                            // record fields
                            let mut segs = w.split('.');
                            let head = segs.next().unwrap_or_default().to_string();
                            let mut e = if head == "arg" {
                                // `arg.x.y` is arg field "x.y"
                                Expr::Arg(w[4..].to_string())
                            } else {
                                Expr::State(head)
                            };
                            for f in segs {
                                e = Expr::Field { base: Box::new(e), name: f.to_string() };
                            }
                            Ok(e)
                        }
                        _ if self.peek().map_or(false, |t| matches!(t, Tok::Op("("))) => {
                            // function call
                            self.next(); // consume (
                            let mut args = Vec::new();
                            if self.peek().map_or(false, |t| matches!(t, Tok::Op(")"))) {
                                self.next();
                            } else {
                                loop {
                                    args.push(self.expr()?);
                                    match self.next() {
                                        Some(Tok::Op(",")) => continue,
                                        Some(Tok::Op(")")) => break,
                                        other => return Err(format!("bad call syntax near {other:?}")),
                                    }
                                }
                            }
                            Ok(Expr::Call { name: w, args })
                        }
                        _ if w.chars().all(|c| c.is_alphanumeric() || c == '_')
                            && w.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') =>
                        {
                            Ok(Expr::State(w))
                        }
                        _ => Err(format!("bad token '{w}'")),
                    },
                    Some(Tok::Op("(")) => {
                        let e = self.expr()?;
                        if self.next().map_or(false, |t| matches!(t, Tok::Op(")"))) {
                            Ok(e)
                        } else {
                            Err("missing ')'".into())
                        }
                    },
                    Some(Tok::Op("-")) => match self.atom()? {
                        Expr::LitInt(i) => Ok(Expr::LitInt(-i)),
                        other => Err(format!("cannot negate {other:?}")),
                    },
                    other => Err(format!("unexpected {other:?}")),
                }
            }
            fn atom_mul(&mut self) -> Result<Expr, String> {
                let mut l = self.atom()?;
                loop {
                    let op = match self.peek() {
                        Some(Tok::Op("*")) => BinOp::Mul,
                        Some(Tok::Op("/")) => BinOp::Div,
                        _ => break,
                    };
                    self.next();
                    let r = self.atom()?;
                    l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
                }
                Ok(l)
            }
            fn atom_add(&mut self) -> Result<Expr, String> {
                let mut l = self.atom_mul()?;
                loop {
                    let op = match self.peek() {
                        Some(Tok::Op("+")) => BinOp::Add,
                        Some(Tok::Op("-")) => BinOp::Sub,
                        _ => break,
                    };
                    self.next();
                    let r = self.atom_mul()?;
                    l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
                }
                Ok(l)
            }
            fn atom_cmp(&mut self) -> Result<Expr, String> {
                let l = self.atom_add()?;
                let op = match self.peek() {
                    Some(Tok::Op("<=")) => BinOp::Le,
                    Some(Tok::Op(">=")) => BinOp::Ge,
                    Some(Tok::Op("==")) => BinOp::Eq,
                    Some(Tok::Op("!=")) => BinOp::Ne,
                    Some(Tok::Op("<")) => BinOp::Lt,
                    Some(Tok::Op(">")) => BinOp::Gt,
                    _ => return Ok(l),
                };
                self.next();
                let r = self.atom_add()?;
                Ok(Expr::Bin { op, l: Box::new(l), r: Box::new(r) })
            }
            fn atom_and(&mut self) -> Result<Expr, String> {
                let mut l = self.atom_cmp()?;
                while self.peek().map_or(false, |t| matches!(t, Tok::Word(w) if w == "and")) {
                    self.next();
                    let r = self.atom_cmp()?;
                    l = Expr::Bin { op: BinOp::And, l: Box::new(l), r: Box::new(r) };
                }
                Ok(l)
            }
            fn expr(&mut self) -> Result<Expr, String> {
                let mut l = self.atom_and()?;
                while self.peek().map_or(false, |t| matches!(t, Tok::Word(w) if w == "or")) {
                    self.next();
                    let r = self.atom_and()?;
                    l = Expr::Bin { op: BinOp::Or, l: Box::new(l), r: Box::new(r) };
                }
                Ok(l)
            }
        }
        let mut p = P { toks: tokenize(src)?, pos: 0 };
        let e = p.expr()?;
        if p.pos != p.toks.len() {
            return Err(format!("trailing tokens in expression '{src}'"));
        }
        Ok(e)
    }
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
    /// §35.6: capabilities this program requires (host-gated)
    pub capabilities: Vec<CapabilityDecl>,
    /// §35.11: declared transport hint (stdio|http|ws|relay)
    pub transport: Option<String>,
    /// §35.8: max concurrent sessions (capacity model)
    pub capacity: Option<usize>,
    /// §35.4: abort conditions — when one becomes true, compensation runs
    pub abort_when: Vec<Guard>,
    /// §35.12: executable scenarios (protocol tests)
    pub scenarios: Vec<Scenario>,
    /// §35.3: user-defined message templates `message name = "text";`
    pub templates: Vec<(String, String)>,
    /// §38.1: user-defined functions `fn f(x: int) -> int { ... }`
    pub functions: Vec<Function>,
    /// §38.2: user-defined record types `type Quote = { price: int };`
    pub types: BTreeMap<String, Vec<(String, String)>>,
    /// §38.5: reusable handler fragments `handler_set name { ... }` —
    /// spliced into a protocol by `include handler_set name;`
    pub handler_sets: BTreeMap<String, Vec<Handler>>,
}

/// §38.1: a user-defined function — pure computation over parameters and
/// session state; parameters and locals are function-scoped.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub ret: Option<String>,
    /// body: `set <local> = <expr>;` + one `return <expr>;`
    pub body: Vec<ProtocolAction>,
    pub returned: Option<Expr>,
}

/// §35.6: capability declaration — the ONLY door to external effects.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityDecl {
    pub name: String,
    pub alias: Option<String>,
}

/// §35.12: scenario — an executable test inside the .aclp file.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub steps: Vec<ScenarioStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScenarioStep {
    /// `<role> sends <perf> [pattern] to <role>;`
    Send { from: String, performative: Performative, pattern: Option<String>, to: String },
    /// `expect <role> sends <perf>;`
    ExpectSend { from: String, performative: Performative },
    /// `expect state <role>.<name>;` (true) / `== false`
    ExpectState { role: String, name: String, value: bool },
    /// `expect commitment <role> <status>;`
    ExpectCommitment { role: String, status: String },
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
/// §38.3: rewrite bare `$var` capture values inside a `{...}` content
/// pattern into JSON string values (`"$var"`) so the pattern is valid
/// JSON — `{"price": $p}` becomes `{"price": "$p"}`.
fn quote_bare_captures(pattern: &str) -> String {
    if !pattern.contains('$') {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len() + 8);
    let cs: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        out.push(c);
        if c == ':' {
            // skip whitespace, then capture a bare `$ident` value
            let mut j = i + 1;
            while j < cs.len() && cs[j].is_whitespace() { j += 1; }
            if j < cs.len() && cs[j] == '$' {
                let mut k = j + 1;
                while k < cs.len() && (cs[k].is_alphanumeric() || cs[k] == '_') { k += 1; }
                if k > j + 1 {
                    // only when the value ends here (comma, whitespace or '}')
                    let end_ok = k >= cs.len()
                        || cs[k] == ','
                        || cs[k] == '}'
                        || cs[k].is_whitespace();
                    if end_ok {
                        out.push('"');
                        out.extend(cs[j..k].iter());
                        out.push('"');
                        i = k - 1; // loop's i += 1 lands on k
                    }
                }
            }
        }
        i += 1;
    }
    out
}

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
            (head[..i].trim().to_string(), Some(quote_bare_captures(&head[i..])))
        }
        _ => (head.trim().to_string(), None),
    };
    (word, pattern, from)
}

/// §35.13: parse with `include "file" with roles (a=X, b=Y);` support —
/// the include is loaded relative to `base_dir`, roles renamed
/// whole-word, and spliced in place of the include line.
pub fn parse_with_includes(src: &str, base_dir: Option<&str>) -> Result<ProtocolProgram, Vec<IdlError>> {
    let mut out_lines: Vec<String> = Vec::new();
    let mut include_errors: Vec<String> = Vec::new();
    for raw in src.lines() {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix("include ") {
            let body = rest.trim_end_matches(';');
            // include "path" [with roles (X=R1, Y=R2)]
            let (path_part, mapping) = match body.find(" with roles ") {
                Some(i) => {
                    let map_src = body[i + " with roles ".len()..]
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')');
                    let mapping: Vec<(String, String)> = map_src
                        .split(',')
                        .filter_map(|p| {
                            let p = p.trim();
                            p.split_once('=')
                                .map(|(a, b)| (b.trim().to_string(), a.trim().to_string()))
                        })
                        .collect();
                    (body[..i].trim(), mapping)
                }
                None => (body.trim(), Vec::new()),
            };
            let path = path_part.trim().trim_matches('"').trim_matches('\'');
            match expand_include(path, &mapping, base_dir) {
                Ok(text) => out_lines.extend(text.lines().map(|l| l.to_string())),
                Err(e) => include_errors.push(e),
            }
        } else {
            out_lines.push(raw.to_string());
        }
    }
    if !include_errors.is_empty() {
        return Err(include_errors
            .into_iter()
            .map(|m| IdlError { line: 0, message: m })
            .collect());
    }
    parse(&out_lines.join("\n"))
}

pub fn parse(src: &str) -> Result<ProtocolProgram, Vec<IdlError>> {
    let mut errors = Vec::new();
    let mut prog = ProtocolProgram::default();
    let mut seen_protocol = false;
    let mut handler: Option<Handler> = None;
    let mut scenario: Option<Scenario> = None;
    let mut func: Option<Function> = None;  // §38.1 WIP
    // §38.5: in-progress handler_set body (name, lines, opener line, depth)
    let mut open_set: Option<(String, Vec<String>, usize, usize)> = None;

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

        // §38.5: inside a handler_set body — collect lines until the
        // closing brace (brace-depth aware for nested handler blocks)
        if let Some((name, body, ln, depth)) = open_set.as_mut() {
            let opens = line.matches('{').count();
            let closes = line.matches('}').count();
            *depth += opens;
            *depth = depth.saturating_sub(closes);
            if line.trim() == "}" && *depth <= 0 {
                let (name, body, ln, _) = open_set.take().unwrap();
                let mini = parse(&format!(
                    "protocol __frag_{name} v1;\nroles __dummy;\n{}",
                    body.join("\n")
                ));
                match mini {
                    Ok(m) => {
                        prog.handler_sets.insert(name, m.handlers);
                    }
                    Err(es) => {
                        for e in es {
                            errors.push(IdlError {
                                line: ln,
                                message: format!("handler_set '{name}': {}", e.message),
                            });
                        }
                    }
                }
            } else {
                body.push(line.to_string());
                if *depth <= 0 {
                    // same-line close — finalize without the brace
                    let (name, body, ln, _) = open_set.take().unwrap();
                    let mini = parse(&format!(
                        "protocol __frag_{name} v1;\nroles __dummy;\n{}",
                        body.join("\n")
                    ));
                    match mini {
                        Ok(m) => {
                            prog.handler_sets.insert(name, m.handlers);
                        }
                        Err(es) => {
                            for e in es {
                                errors.push(IdlError {
                                    line: ln,
                                    message: format!("handler_set '{name}': {}", e.message),
                                });
                            }
                        }
                    }
                }
            }
            continue;
        }
        // §38.5: handler_set opener — `handler_set name {` or inline
        if let Some(rest) = line.strip_prefix("handler_set ") {
            let name = rest.split('{').next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                errors.push(IdlError { line: line_no, message: "handler_set needs a name".into() });
            } else if line.contains('{') && line.ends_with('}') {
                let inner = &line[line.find('{').unwrap() + 1..line.len() - 1];
                let mini = parse(&format!(
                    "protocol __frag_{name} v1;\nroles __dummy;\n{inner}"
                ));
                match mini {
                    Ok(m) => {
                        prog.handler_sets.insert(name, m.handlers);
                    }
                    Err(es) => {
                        for e in es {
                            errors.push(IdlError {
                                line: line_no,
                                message: format!("handler_set '{name}': {}", e.message),
                            });
                        }
                    }
                }
            } else if line.ends_with('{') {
                open_set = Some((name, Vec::new(), line_no, 1));
            } else {
                errors.push(IdlError { line: line_no, message: "malformed handler_set (want 'handler_set name {')".into() });
            }
            continue;
        }
        // §38.5: splice a declared handler set into the current protocol
        if let Some(rest) = line.strip_prefix("include handler_set ") {
            let name = rest.trim_end_matches(';').trim().to_string();
            match prog.handler_sets.get(&name) {
                Some(hs) => prog.handlers.extend(hs.iter().cloned()),
                None => errors.push(IdlError {
                    line: line_no,
                    message: format!(
                        "unknown handler set '{name}' (declare it with handler_set before include)"
                    ),
                }),
            }
            continue;
        }

        // §38.1: inside a function body — collect statements until `}`
        if let Some(f) = func.as_mut() {
            if line == "}" {
                let f = func.take().unwrap();
                if f.returned.is_none() {
                    errors.push(IdlError {
                        line: line_no,
                        message: format!("fn '{}' has no return statement", f.name),
                    });
                }
                prog.functions.push(f);
                continue;
            }
            match parse_action(line) {
                Ok(ProtocolAction::Set { name, field, expr }) => {
                    if field.is_some() {
                        errors.push(IdlError {
                            line: line_no,
                            message: format!("field assignment '{}' not allowed inside fn", name),
                        });
                        continue;
                    }
                    f.body.push(ProtocolAction::Set { name, field, expr });
                }
                Ok(ProtocolAction::Return { expr }) => {
                    f.returned = Some(expr);
                }
                Ok(other) => errors.push(IdlError {
                    line: line_no,
                    message: format!("only 'set' and 'return' allowed in fn body, got {other:?}"),
                }),
                Err(e) => errors.push(IdlError { line: line_no, message: e }),
            }
            continue;
        }

        // §38.1: `fn name(p1: t1, p2: t2) -> ret {`
        if line.starts_with("fn ") && line.ends_with('{') {
            let head = &line[3..line.len() - 1];
            let (sig, ret) = match head.split_once("->") {
                Some((a, b)) => (a.trim(), Some(b.trim().to_string())),
                None => (head.trim(), None),
            };
            let (name, params_src) = match sig.split_once('(') {
                Some((n, p)) => (n.trim().to_string(), p.trim_end_matches(')').trim().to_string()),
                None => {
                    errors.push(IdlError { line: line_no, message: format!("bad fn decl: '{line}'") });
                    continue;
                }
            };
            let params: Vec<(String, String)> = if params_src.is_empty() {
                Vec::new()
            } else {
                params_src
                    .split(',')
                    .filter_map(|p| {
                        let p = p.trim();
                        p.split_once(':').map(|(n, t)| (n.trim().to_string(), t.trim().to_string()))
                    })
                    .collect()
            };
            func = Some(Function { name, params, ret, body: Vec::new(), returned: None });
            continue;
        }

        // §38.2: `type Name = { field: ty, ... };`
        if let Some(rest) = line.strip_prefix("type ") {
            let body = rest.trim_end_matches(';').trim();
            if let Some((name, fields_src)) = body.split_once('=') {
                let name = name.trim().to_string();
                let inner = fields_src.trim().trim_start_matches('{').trim_end_matches('}').trim();
                let fields: Vec<(String, String)> = inner
                    .split(',')
                    .filter(|f| !f.trim().is_empty())
                    .filter_map(|f| {
                        let f = f.trim();
                        f.split_once(':').map(|(n, t)| (n.trim().to_string(), t.trim().to_string()))
                    })
                    .collect();
                if fields.is_empty() || name.is_empty() {
                    errors.push(IdlError { line: line_no, message: format!("bad type decl: '{line}'") });
                } else {
                    prog.types.insert(name, fields);
                }
            } else {
                errors.push(IdlError { line: line_no, message: format!("bad type decl: '{line}' (want 'type X = {{ f: ty }};')") });
            }
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
            // §35.3: inline for-block: `for x in list { act; act; }`
            if line.starts_with("for ") && line.ends_with('}') {
                match parse_for_block(line) {
                    Ok(a) => h.actions.push(a),
                    Err(e) => errors.push(IdlError { line: line_no, message: e }),
                }
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
            // inline block: `on X { act; act; }` — split the block body off
            // BEFORE trigger parsing (a content pattern's braces must not
            // leak into the trigger's from-clause)
            let mut block_body: Option<String> = None;
            let head = if !arrow && rest.ends_with('}') && rest.contains('{') {
                let i = rest.rfind('{').unwrap();
                block_body = Some(rest[i + 1..rest.len() - 1].to_string());
                rest[..i].trim()
            } else {
                head
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
            // §38.4: alternation `on (accept | reject) from a` — expands to
            // one handler per alternative with identical guard and actions.
            // Only valid in one-line and inline-block forms.
            let expanded: Vec<String> = if head.starts_with('(') {
                match head.find(')') {
                    Some(close) if close > 1 => {
                        let alts: Vec<String> = head[1..close]
                            .split('|')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let rest = head[close + 1..].trim().to_string();
                        if alts.len() > 1 {
                            alts.into_iter()
                                .map(|a| if rest.is_empty() { a.clone() } else { format!("{a} {rest}") })
                                .collect()
                        } else {
                            vec![head.to_string()]
                        }
                    }
                    _ => vec![head.to_string()],
                }
            } else {
                vec![head.to_string()]
            };
            let inline_block = block_body.is_some();
            if expanded.len() > 1 && !arrow && !inline_block {
                errors.push(IdlError {
                    line: line_no,
                    message: "alternation requires one-line or inline-block form".into(),
                });
                continue;
            }
            let mut hs: Vec<Handler> = Vec::new();
            for hhead in &expanded {
                let trigger = match parse_trigger(hhead) {
                    Ok(t) => t,
                    Err(e) => {
                        errors.push(IdlError { line: line_no, message: e });
                        continue;
                    }
                };
                hs.push(Handler { trigger, guard: guard.clone(), actions: Vec::new() });
            }
            if hs.is_empty() {
                continue;
            }
            if arrow {
                // one-line form: `on X -> send Y;` (no block)
                let stmt = sends.trim_end_matches(';').trim();
                match parse_action(stmt) {
                    Ok(a) => {
                        for h in &mut hs {
                            h.actions.push(a.clone());
                        }
                    }
                    Err(e) => errors.push(IdlError { line: line_no, message: e }),
                }
                prog.handlers.extend(hs);
            } else if line.ends_with('{') {
                handler = Some(hs.remove(0));
            } else if let Some(body_src) = block_body.take() {
                // inline block: `on X { act; act; }` — body already split off
                let body = body_src;
                for stmt in body.split(';') {
                    if stmt.trim().is_empty() {
                        continue;
                    }
                    match parse_action(stmt.trim()) {
                        Ok(a) => {
                            for hh in &mut hs {
                                hh.actions.push(a.clone());
                            }
                        }
                        Err(e) => {
                            errors.push(IdlError { line: line_no, message: e });
                            break;
                        }
                    }
                }
                for h in hs {
                    if !h.actions.is_empty() {
                        prog.handlers.push(h);
                    }
                }
            } else {
                errors.push(IdlError { line: line_no, message: format!("malformed handler: '{line}' (want 'on <event> -> <action>;' or 'on <event> {{')") });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("message ") {
            let body = rest.trim_end_matches(';').trim();
            match body.split_once('=') {
                Some((name, text)) => {
                    let name = name.trim().to_string();
                    let text = text.trim().trim_matches('"').to_string();
                    if name.is_empty() || text.is_empty() {
                        errors.push(IdlError { line: line_no, message: format!("bad message template: '{line}'") });
                    } else {
                        prog.templates.push((name, text));
                    }
                }
                None => errors.push(IdlError { line: line_no, message: format!("bad message template: '{line}' (want 'message name = \"text\";')") }),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("use ") {
            let body = rest.trim_end_matches(';').trim();
            let (name, alias) = match body.split_once(" as ") {
                Some((n, a)) => (n.trim().to_string(), Some(a.trim().to_string())),
                None => (body.to_string(), None),
            };
            prog.capabilities.push(CapabilityDecl { name, alias });
            continue;
        }
        if let Some(rest) = line.strip_prefix("transport ") {
            let t = rest.trim_end_matches(';').trim().to_string();
            if !["stdio", "http", "ws", "relay"].contains(&t.as_str()) {
                errors.push(IdlError {
                    line: line_no,
                    message: format!("unknown transport '{t}' (want stdio|http|ws|relay)"),
                });
            }
            prog.transport = Some(t);
            continue;
        }
        if let Some(rest) = line.strip_prefix("capacity ") {
            match rest.trim_end_matches(';').trim().parse::<usize>() {
                Ok(n) if n > 0 => prog.capacity = Some(n),
                _ => errors.push(IdlError { line: line_no, message: format!("bad capacity: '{line}'") }),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("abort when ") {
            match parse_guard(rest.trim_end_matches(';').trim()) {
                Ok(g) => prog.abort_when.push(g),
                Err(e) => errors.push(IdlError { line: line_no, message: format!("bad abort guard: {e}") }),
            }
            continue;
        }
        if line.starts_with("scenario ") && line.ends_with('{') {
            scenario = Some(Scenario {
                name: line[8..line.len() - 1].trim().trim_matches('"').to_string(),
                steps: Vec::new(),
            });
            continue;
        }
        if let Some(sc) = scenario.as_mut() {
            if line == "}" {
                if sc.steps.is_empty() {
                    errors.push(IdlError { line: line_no, message: "empty scenario".into() });
                }
                prog.scenarios.push(scenario.take().unwrap());
                continue;
            }
            match parse_scenario_step(line) {
                Ok(st) => sc.steps.push(st),
                Err(e) => errors.push(IdlError { line: line_no, message: e }),
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
    if scenario.is_some() {
        errors.push(IdlError { line: 0, message: "unterminated scenario block".into() });
    }
    if func.is_some() {
        errors.push(IdlError { line: 0, message: "unterminated fn block (missing '}')".into() });
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

/// §35.3: `for x in list { send inform to x; set done = true; }`
fn parse_for_block(line: &str) -> Result<ProtocolAction, String> {
    let inner = line
        .strip_prefix("for ")
        .ok_or("not a for")?
        .trim_end_matches('}')
        .trim();
    let (head, body) = inner
        .split_once('{')
        .ok_or("for needs a block")?;
    let (var, over) = head
        .trim()
        .split_once(" in ")
        .ok_or("for needs 'for var in list'")?;
    let actions: Vec<ProtocolAction> = body
        .split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| parse_action(p.trim()))
        .collect::<Result<_, _>>()?;
    Ok(ProtocolAction::For {
        var: var.trim().to_string(),
        over: over.trim().to_string(),
        actions,
    })
}

/// §35.12: one scenario step
fn parse_scenario_step(line: &str) -> Result<ScenarioStep, String> {
    let s = line.trim().trim_end_matches(';').trim();
    if let Some(rest) = s.strip_prefix("expect ") {
        if let Some(rest) = rest.strip_prefix("state ") {
            // expect state <role>.<name> [== <bool>]
            let (ref_part, val) = match rest.split_once("==") {
                Some((a, b)) => (a.trim(), b.trim() == "true"),
                None => (rest, true),
            };
            let (role, name) = ref_part
                .split_once('.')
                .ok_or("expect state needs role.name")?;
            return Ok(ScenarioStep::ExpectState {
                role: role.trim().to_string(),
                name: name.trim().to_string(),
                value: val,
            });
        }
        if let Some(rest) = rest.strip_prefix("commitment ") {
            let (role, status) = rest
                .split_once(' ')
                .ok_or("expect commitment needs role status")?;
            return Ok(ScenarioStep::ExpectCommitment {
                role: role.trim().to_string(),
                status: status.trim().to_string(),
            });
        }
        // expect <role> sends <perf>
        let (role, perf) = rest
            .split_once(" sends ")
            .ok_or("expect sends needs 'role sends perf'")?;
        let p = perf_from_word(perf.trim()).ok_or(format!("unknown performative '{perf}'"))?;
        return Ok(ScenarioStep::ExpectSend { from: role.trim().to_string(), performative: p });
    }
    // <role> sends <perf> [pattern] to <role>
    let (head, to) = s.split_once(" to ").ok_or("scenario send needs 'to <role>'")?;
    let (from, rest) = head
        .split_once(" sends ")
        .ok_or("scenario step must be 'X sends Y to Z' or 'expect ...'")?;
    let rest = rest.trim();
    // peel an optional pattern: `request("q")` or `request {...}`
    let (perf_word, pattern) = match rest.find(|c| c == '(' || c == '{') {
        Some(i) => (rest[..i].trim(), Some(rest[i..].to_string())),
        None => (rest, None),
    };
    let p = perf_from_word(perf_word).ok_or(format!("unknown performative '{perf_word}'"))?;
    Ok(ScenarioStep::Send {
        from: from.trim().to_string(),
        performative: p,
        pattern,
        to: to.trim().to_string(),
    })
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
    if let Some(rest) = s.strip_prefix("set ") {
        let (name, expr) = rest
            .split_once('=')
            .ok_or_else(|| format!("bad set: '{s}' (want 'set x = expr')"))?;
        let expr = Expr::parse_expr(expr.trim())?;
        // `set q.price = ...` targets a record field (§38.2)
        let (name, field) = match name.trim().split_once('.') {
            Some((base, f)) => (base.trim().to_string(), Some(f.trim().to_string())),
            None => (name.trim().to_string(), None),
        };
        return Ok(ProtocolAction::Set { name, field, expr });
    }
    if let Some(rest) = s.strip_prefix("return ") {
        let expr = Expr::parse_expr(rest.trim())?;
        return Ok(ProtocolAction::Return { expr });
    }
    if s == "return" {
        return Ok(ProtocolAction::Return { expr: Expr::LitBool(true) });
    }
    if let Some(rest) = s.strip_prefix("push ") {
        let parts: Vec<&str> = rest.splitn(2, ',').collect();
        if parts.len() != 2 {
            return Err(format!("bad push: '{s}' (want 'push list, expr')"));
        }
        let expr = Expr::parse_expr(parts[1].trim())?;
        return Ok(ProtocolAction::Push { state: parts[0].trim().to_string(), expr });
    }
    if let Some(rest) = s.strip_prefix("put ") {
        let parts: Vec<&str> = rest.splitn(3, ',').collect();
        if parts.len() != 3 {
            return Err(format!("bad put: '{s}' (want 'put map, key, expr')"));
        }
        let expr = Expr::parse_expr(parts[2].trim())?;
        return Ok(ProtocolAction::Put {
            state: parts[0].trim().to_string(),
            key: parts[1].trim().trim_matches('"').to_string(),
            expr,
        });
    }
    if s == "abort" {
        return Ok(ProtocolAction::Abort);
    }
    if let Some(rest) = s.strip_prefix("send ") {
        // peel retry clause first: `retry 3 backoff 2s exp`
        let (rest, retry) = match rest.find(" retry ") {
            Some(i) => {
                let clause = rest[i..].trim();
                let head = rest[..i].trim();
                // parse: retry N backoff D [exp]
                let parts: Vec<&str> = clause.split_whitespace().collect();
                let retry = match parts.as_slice() {
                    ["retry", n, "backoff", d] => Retry {
                        attempts: n.parse().map_err(|_| "bad retry count")?,
                        backoff_ms: parse_duration(d).ok_or("bad backoff duration")?,
                        exponential: false,
                    },
                    ["retry", n, "backoff", d, "exp"] => Retry {
                        attempts: n.parse().map_err(|_| "bad retry count")?,
                        backoff_ms: parse_duration(d).ok_or("bad backoff duration")?,
                        exponential: true,
                    },
                    _ => return Err(format!("bad retry clause: '{clause}' (want 'retry N backoff D [exp]')")),
                };
                (head, Some(retry))
            }
            None => (rest, None),
        };
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
            Some(p) => Ok(ProtocolAction::Send { performative: p, to, fields, retry }),
            None => Err(format!("unknown performative in send: '{word}'")),
        };
    }
    Err(format!("unknown statement: '{s}'"))
}

// ---------------------------------------------------------------- check

/// §38.6: static type of an expression — Some only when statically known
/// ("int" | "float" | "str" | "bool" | declared record type name).
pub fn static_ty(e: &Expr, prog: &ProtocolProgram) -> Option<String> {
    match e {
        Expr::LitInt(_) => Some("int".into()),
        Expr::LitStr(_) => Some("str".into()),
        Expr::LitBool(_) => Some("bool".into()),
        Expr::State(n) => prog
            .states
            .iter()
            .find(|s| &s.name == n)
            .and_then(|s| s.ty.clone()),
        Expr::Field { base, name } => {
            let bt = static_ty(base, prog)?;
            prog.types
                .get(&bt)?
                .iter()
                .find(|(f, _)| f == name)
                .map(|(_, t)| t.clone())
        }
        Expr::RecLit { ty, .. } => Some(ty.clone()),
        Expr::Call { name, .. } => prog
            .functions
            .iter()
            .find(|f| &f.name == name)
            .and_then(|f| f.ret.clone()),
        Expr::Not(_) => Some("bool".into()),
        Expr::Bin { op, l, r } => match op {
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
            | BinOp::And | BinOp::Or => Some("bool".into()),
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let lt = static_ty(l, prog);
                let rt = static_ty(r, prog);
                match (lt, rt) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    (Some(a), Some(b))
                        if (a == "int" && b == "float") || (a == "float" && b == "int") =>
                    {
                        Some("float".into())
                    }
                    _ => None,
                }
            }
        },
        Expr::Capture(_) | Expr::Arg(_) => None,
    }
}

/// §38.6: widening rule — int widens to float, nothing else coerces.
fn ty_assign_ok(declared: &str, got: &str) -> bool {
    declared == got || (declared == "float" && got == "int")
}

/// §38.6: walk assignment actions; check declared state types against
/// inferred expression types and record field existence (recursively
/// through `for` bodies).
fn check_action_types(
    prog: &ProtocolProgram,
    actions: &[ProtocolAction],
    errors: &mut Vec<IdlError>,
) {
    for a in actions {
        match a {
            ProtocolAction::Set { name, field, expr } => {
                if let Some(f) = field {
                    // field write `set q.price = ...`: q must be a record
                    // and 'price' one of its declared fields
                    let declared = prog
                        .states
                        .iter()
                        .find(|s| &s.name == name)
                        .and_then(|s| s.ty.clone());
                    if let Some(dt) = declared {
                        match prog.types.get(&dt) {
                            Some(fields) => {
                                if !fields.iter().any(|(fname, _)| fname == f) {
                                    errors.push(IdlError {
                                        line: 0,
                                        message: format!(
                                            "record '{dt}' has no field '{f}' (set {name}.{f})"
                                        ),
                                    });
                                }
                            }
                            None => errors.push(IdlError {
                                line: 0,
                                message: format!(
                                    "'{dt}' is not a declared record type (set {name}.{f})"
                                ),
                            }),
                        }
                    }
                } else if let Some(dt) = prog
                    .states
                    .iter()
                    .find(|s| &s.name == name)
                    .and_then(|s| s.ty.clone())
                {
                    if let Some(gt) = static_ty(expr, prog) {
                        if !ty_assign_ok(&dt, &gt) {
                            errors.push(IdlError {
                                line: 0,
                                message: format!(
                                    "type mismatch: state '{name}' is '{dt}' but expression is '{gt}'"
                                ),
                            });
                        }
                    }
                }
                check_expr_fields(prog, expr, errors);
            }
            ProtocolAction::Push { expr, .. } | ProtocolAction::Put { expr, .. } => {
                check_expr_fields(prog, expr, errors);
            }
            ProtocolAction::For { actions, .. } => {
                check_action_types(prog, actions, errors)
            }
            _ => {}
        }
    }
}

/// §38.6: recursively check record field access/literals inside an expr.
fn check_expr_fields(prog: &ProtocolProgram, e: &Expr, errors: &mut Vec<IdlError>) {
    match e {
        Expr::Field { base, name } => {
            if let Some(bt) = static_ty(base, prog) {
                match prog.types.get(&bt) {
                    Some(fields) => {
                        if !fields.iter().any(|(f, _)| f == name) {
                            errors.push(IdlError {
                                line: 0,
                                message: format!("record '{bt}' has no field '{name}'"),
                            });
                        }
                    }
                    None => {
                        if matches!(bt.as_str(), "int" | "str" | "bool" | "float") {
                            errors.push(IdlError {
                                line: 0,
                                message: format!("'{bt}' is not a record — no field '{name}'"),
                            });
                        }
                    }
                }
            }
            check_expr_fields(prog, base, errors);
        }
        Expr::Bin { l, r, .. } => {
            check_expr_fields(prog, l, errors);
            check_expr_fields(prog, r, errors);
        }
        Expr::Not(x) => check_expr_fields(prog, x, errors),
        Expr::Call { args, .. } => {
            for a in args {
                check_expr_fields(prog, a, errors);
            }
        }
        Expr::RecLit { ty, fields } => {
            if let Some(decl) = prog.types.get(ty) {
                for (f, _) in fields {
                    if !decl.iter().any(|(df, _)| df == f) {
                        errors.push(IdlError {
                            line: 0,
                            message: format!("record '{ty}' has no field '{f}'"),
                        });
                    }
                }
            }
            for (_, fe) in fields {
                check_expr_fields(prog, fe, errors);
            }
        }
        _ => {}
    }
}

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

    // ---- §35: set/push/put/for targets must be declared states ----
    for h in &prog.handlers {
        for a in &h.actions {
            match a {
                ProtocolAction::Set { name, .. } | ProtocolAction::Push { state: name, .. }
                | ProtocolAction::Put { state: name, .. } => {
                    if !prog.states.iter().any(|st| &st.name == name) {
                        errors.push(IdlError {
                            line: 0,
                            message: format!("'{name}' is not a declared state (set/push/put)"),
                        });
                    }
                }
                ProtocolAction::For { var, over, .. } => {
                    if !prog.states.iter().any(|st| &st.name == over) {
                        errors.push(IdlError {
                            line: 0,
                            message: format!("for over '{over}': not a declared state"),
                        });
                    }
                    if !prog.states.iter().any(|st| &st.name == var) && !var.starts_with('$') {
                        warnings.push(format!(
                            "for var '{var}' is not a declared state — it is scoped to the loop"
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    // ---- §38.6: static type checks — assignments & record field access ----
    for h in &prog.handlers {
        check_action_types(prog, &h.actions, &mut errors);
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
        // §38.3: expression guards render back as their expression
        Guard::ExprGuard(e) => expr_word(e),
    }
}

fn action_word(a: &ProtocolAction) -> String {
    match a {
        ProtocolAction::Send { performative, to, fields, .. } => {
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
        ProtocolAction::Set { name, field, expr } => format!(
            "set {}{} = {}",
            name,
            match field { Some(f) => format!(".{f}"), None => String::new() },
            expr_word(expr),
        ),
        ProtocolAction::Push { state, expr } => format!("push {}, {}", state, expr_word(expr)),
        ProtocolAction::Put { state, key, expr } => format!("put {}, {}, {}", state, key, expr_word(expr)),
        ProtocolAction::For { var, over, actions } => {
            let inner: Vec<String> = actions.iter().map(|a| action_word(a)).collect();
            format!("for {} in {} {{ {} }}", var, over, inner.join("; "))
        }
        ProtocolAction::Abort => "abort".into(),
        ProtocolAction::Return { expr } => format!("return {}", expr_word(expr)),
    }
}

fn expr_word(e: &Expr) -> String {
    match e {
        Expr::LitInt(i) => i.to_string(),
        Expr::LitStr(s) => format!("\"{s}\""),
        Expr::LitBool(b) => b.to_string(),
        Expr::State(n) => n.clone(),
        Expr::Capture(n) => format!("${n}"),
        Expr::Arg(a) => format!("arg.{a}"),
        Expr::Bin { op, l, r } => {
            let o = match op {
                BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*", BinOp::Div => "/",
                BinOp::Lt => "<", BinOp::Le => "<=", BinOp::Gt => ">", BinOp::Ge => ">=",
                BinOp::Eq => "==", BinOp::Ne => "!=", BinOp::And => "and", BinOp::Or => "or",
            };
            format!("{} {} {}", expr_word(l), o, expr_word(r))
        }
        Expr::Not(x) => format!("not {}", expr_word(x)),
        Expr::Call { name, args } => {
            let a: Vec<String> = args.iter().map(expr_word).collect();
            format!("{}({})", name, a.join(", "))
        }
        Expr::Field { base, name } => format!("{}.{}", expr_word(base), name),
        Expr::RecLit { ty, fields } => {
            let f: Vec<String> = fields.iter().map(|(n, e)| format!("{}: {}", n, expr_word(e))).collect();
            format!("{} {{ {} }}", ty, f.join(", "))
        }
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
    fn computation_statements_parse_and_check() {
        let src = r#"
protocol p v1;
roles a, b;
state total: int;
state bids: list;
state idx: int;
on request from b {
  set total = total + 1;
  push bids, arg.price;
  put idx, "last", total;
  for x in bids { set total = total + len(bids); }
}
on inform from b -> send bye;
"#;
        let p = parse(src).expect("computation statements parse");
        check(&p).expect("targets declared");
    }

    #[test]
    fn set_undeclared_state_is_error() {
        let bad = parse("protocol p v1;\nroles a,b;\non query from a { set nope = 1; }\non inform from b -> send bye;\n").unwrap();
        let errs = check(&bad).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("'nope' is not a declared state")));
    }

    #[test]
    fn retry_clause_parses() {
        let src = "protocol p v1;\nroles a, b;\non query from a -> send inform to a retry 3 backoff 2s exp;\non inform from b -> send bye;\n";
        let p = parse(src).unwrap();
        match &p.handlers[0].actions[0] {
            ProtocolAction::Send { retry: Some(r), .. } => {
                assert_eq!(r.attempts, 3);
                assert_eq!(r.backoff_ms, 2000);
                assert!(r.exponential);
            }
            other => panic!("expected retry, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_transport_capacity_parse() {
        let src = r#"
protocol p v1;
roles a, b;
use fs.read as reader;
use net.http;
transport relay;
capacity 5;
on query from a -> send inform to a;
on inform from b -> send bye;
"#;
        let p = parse(src).unwrap();
        assert_eq!(p.capabilities.len(), 2);
        assert_eq!(p.capabilities[0].alias.as_deref(), Some("reader"));
        assert_eq!(p.transport.as_deref(), Some("relay"));
        assert_eq!(p.capacity, Some(5));
        check(&p).expect("ok");
        // unknown transport
        assert!(parse("protocol p v1;\nroles a,b;\ntransport carrier_pigeon;\non query from a -> send inform to a;\non inform from b -> send bye;\n").is_err());
        assert!(parse("protocol p v1;\nroles a,b;\ntransport pigeon;\non query from a -> send inform to a;\non inform from b -> send bye;\n").is_err());
    }

    #[test]
    fn abort_when_and_abort_statement_parse() {
        let src = r#"
protocol p v1;
roles a, b;
state quoted: bool;
state paid: bool;
abort when quoted and not paid;
on request from b -> send propose to b;
on accept from b {
  abort;
}
on propose from a -> send bye;
"#;
        let p = parse(src).unwrap();
        assert_eq!(p.abort_when.len(), 1);
        assert!(p.handlers.iter().any(|h| h.actions.iter().any(|a| matches!(a, ProtocolAction::Abort))));
        check(&p).expect("ok");
    }

    #[test]
    fn scenarios_parse() {
        let src = r#"
protocol p v1;
roles a, b;
on query from a -> send inform to a;
on inform from b -> send bye;

scenario "q&a" {
  a sends query to b;
  expect b sends inform;
  expect a sends bye;
}
"#;
        let p = parse(src).unwrap();
        assert_eq!(p.scenarios.len(), 1);
        assert_eq!(p.scenarios[0].name, "q&a");
        assert_eq!(p.scenarios[0].steps.len(), 3);
    }

    #[test]
    fn message_templates_parse() {
        let src = "protocol p v1;\nroles a,b;\nmessage invoice = \"INVOICE {0} for {1}\";\non query from a -> send inform to a;\non inform from b -> send bye;\n";
        let p = parse(src).unwrap();
        assert_eq!(p.templates, vec![("invoice".into(), "INVOICE {0} for {1}".into())]);
    }

    #[test]
    fn unterminated_block_and_missing_protocol() {
        assert!(parse("on query -> send inform;").is_err());
        assert!(parse("protocol p v1;\nroles a;\non query {").is_err());
        let e = parse("roles a;").unwrap_err();
        assert!(e.iter().any(|x| x.message.contains("missing 'protocol")));
    }
}

/// expand an include: load `path` (relative to `base_dir`), rename roles
/// per mapping (whole-word), return its text.
fn expand_include(path: &str, mapping: &[(String, String)], base_dir: Option<&str>) -> Result<String, String> {
    let full = match base_dir {
        Some(d) => format!("{d}/{path}"),
        None => path.to_string(),
    };
    let text = std::fs::read_to_string(&full)
        .map_err(|e| format!("include '{path}': {e}"))?;
    let mut out = text;
    for (from, to) in mapping {
        // whole-word rename
        let pattern = format!(r"\b{}\b", regex_escape(from));
        out = regex_replace(&out, &pattern, to);
    }
    Ok(out)
}

fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_string() } else { format!("\\{c}") }).collect()
}

fn regex_replace(text: &str, pattern: &str, replacement: &str) -> String {
    // minimal whole-word replacer: split on non-word chars, rebuild
    let word = pattern.trim_start_matches(r"\b").trim_end_matches(r"\b");
    let mut out = String::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            out.push_str(if cur == word { replacement } else { &cur });
            out.push(c);
            cur.clear();
        }
    }
    out.push_str(if cur == word { replacement } else { &cur });
    out
}

#[cfg(test)]
mod alternation_tests {
    use super::*;

    // §38.4: alternation expands to one handler per alternative
    #[test]
    fn alternation_expands_handlers() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non (accept | reject) from a -> send inform to a;\non inform from b -> send bye;\n",
        )
        .unwrap();
        let alts: Vec<_> = prog
            .handlers
            .iter()
            .filter(|h| match &h.trigger {
                OnTrigger::Event { performative, from, .. } => {
                    *from == Some("a".to_string())
                        && matches!(performative, Performative::Accept | Performative::Reject)
                }
                _ => false,
            })
            .collect();
        assert_eq!(alts.len(), 2);
        // both handlers share the same action
        assert_eq!(alts[0].actions, alts[1].actions);
        assert!(check(&prog).is_ok());
    }

    #[test]
    fn alternation_with_guard_expands_guard() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\non (inform | query) from a when quoted -> send inform to a;\n",
        )
        .unwrap();
        assert_eq!(prog.handlers.len(), 2);
        assert!(prog.handlers[0].guard.is_some());
        assert_eq!(prog.handlers[0].guard, prog.handlers[1].guard);
    }

    #[test]
    fn literal_paren_pattern_not_alternation() {
        // `("quote")` is a literal pattern, not an alternation (single alt)
        let prog = parse(
            "protocol p v1;\nroles a, b;\non request (\"quote\") from a -> send inform to a;\n",
        )
        .unwrap();
        assert_eq!(prog.handlers.len(), 1);
    }

    #[test]
    fn alternation_block_form_rejected() {
        let errs = parse(
            "protocol p v1;\nroles a, b;\non (accept | reject) from a {\n  begin;\n}\n",
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("alternation")));
    }
}

#[cfg(test)]
mod handler_set_tests {
    use super::*;

    // §38.5: handler_set fragments splice into a protocol via include
    #[test]
    fn handler_set_splices_handlers() {
        let prog = parse(
            "handler_set common_failure {\non breach -> send failure to buyer;\non expired -> send error to buyer;\n}\nprotocol p v1;\nroles buyer, seller;\non request(\"quote\") from buyer -> send propose to buyer;\non propose from seller -> send accept to seller;\non accept from buyer -> begin;\non accept from seller -> send bye;\ninclude handler_set common_failure;\non failure from seller -> send bye;\non error from seller -> send bye;\n",
        )
        .unwrap();
        assert!(prog.handler_sets.contains_key("common_failure"));
        assert_eq!(prog.handler_sets["common_failure"].len(), 2);
        // 6 own handlers + 2 spliced
        assert_eq!(prog.handlers.len(), 8);
        assert!(check(&prog).is_ok());
    }

    #[test]
    fn handler_set_unknown_include_errors() {
        let errs = parse(
            "protocol p v1;\nroles a, b;\non inform from a -> send inform to b;\non inform from b -> send bye;\ninclude handler_set nosuch;\n",
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("unknown handler set")));
    }

    #[test]
    fn handler_set_roles_checked_at_include() {
        // the fragment references role 'ghost' which the including
        // protocol does not declare — check() must catch it
        let prog = parse(
            "handler_set bad {\non breach -> send failure to ghost;\n}\nprotocol p v1;\nroles a, b;\non inform from a -> send inform to b;\non inform from b -> send bye;\ninclude handler_set bad;\non breach -> send bye;\n",
        )
        .unwrap();
        assert!(check(&prog).is_err());
    }

    #[test]
    fn handler_set_inline_form() {
        let prog = parse(
            "handler_set tiny { on breach -> send failure to a; }\nprotocol p v1;\nroles a, b;\non inform from a -> send inform to b;\non inform from b -> send bye;\non breach -> send bye;\ninclude handler_set tiny;\n",
        )
        .unwrap();
        assert_eq!(prog.handler_sets["tiny"].len(), 1);
        assert_eq!(prog.handlers.len(), 4);
    }
}

#[cfg(test)]
mod static_type_tests {
    use super::*;

    fn check_errs(src: &str) -> Vec<String> {
        let prog = parse(src).unwrap();
        check(&prog)
            .unwrap_err()
            .into_iter()
            .map(|e| e.message)
            .collect()
    }

    #[test]
    fn set_type_mismatch_caught() {
        let errs = check_errs(
            "protocol p v1;\nroles a, b;\nstate total: int;\nstate flag: bool;\non inform from a { set total = 1 + 2; set flag = 3; set total = \"x\"; send bye; }\n",
        );
        assert!(errs.iter().any(|m| m.contains("state 'flag' is 'bool'")), "{errs:?}");
        assert!(errs.iter().any(|m| m.contains("state 'total' is 'int'") && m.contains("'str'")), "{errs:?}");
        // 1 + 2 → int → total:int OK — no error for it
        assert!(!errs.iter().any(|m| m.contains("'total' is 'int' but expression is 'int'")), "{errs:?}");
    }

    #[test]
    fn arg_and_capture_stay_dynamic() {
        // arg.* and $captures are statically unknown — must NOT error
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate total: int;\non inform {\"amount\": $v} from a { set total = $v; send bye; }\n",
        )
        .unwrap();
        assert!(check(&prog).is_ok());
    }

    #[test]
    fn record_field_access_checked() {
        let errs = check_errs(
            "type Quote = { price: int, currency: str };\nprotocol p v1;\nroles a, b;\nstate q: Quote;\nstate bad: int;\non inform from a { set q.wrong = 5; set bad = q.price + 1; set bad = q.price.nope; send bye; }\n",
        );
        assert!(errs.iter().any(|m| m.contains("'Quote' has no field 'wrong'")), "{errs:?}");
        assert!(!errs.iter().any(|m| m.contains("q.price + 1") || (m.contains("no field 'price'"))), "{errs:?}");
        assert!(errs.iter().any(|m| m.contains("no field 'nope'")), "{errs:?}");
    }

    #[test]
    fn field_on_scalar_rejected() {
        let errs = check_errs(
            "protocol p v1;\nroles a, b;\nstate total: int;\non inform from a { set total = total.price; send bye; }\n",
        );
        assert!(errs.iter().any(|m| m.contains("not a record")), "{errs:?}");
    }

    #[test]
    fn int_widens_to_float() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate r: float;\non inform from a { set r = 1 + 2; send bye; }\n",
        )
        .unwrap();
        assert!(check(&prog).is_ok());
    }

    #[test]
    fn bool_expr_types_check() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate flag: bool;\non inform from a { set flag = 1 < 2 and not (3 == 4); send bye; }\n",
        )
        .unwrap();
        assert!(check(&prog).is_ok());
    }
}
