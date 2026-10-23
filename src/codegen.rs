//! §34: endpoint codegen — `aclc gen agent py <file.aclp> <out_dir>`.
//!
//! Emits a RUNNABLE Python skeleton per role: the projected handlers as
//! data, the dispatch loop, guard evaluation, and timer ticking. The
//! generated agent has no ACL-specific dependencies — it speaks the
//! protocol the program declares, exactly as compiled.

use crate::protocol::{BinOp, Expr, Guard, OnTrigger, ProtocolAction, ProtocolProgram};
use crate::runtime::{project, RoleEndpoint};
use crate::Performative;

fn py_perf(p: &Performative) -> String {
    format!("{:?}", p).to_lowercase()
}

fn py_guard(g: &Guard) -> String {
    match g {
        Guard::Atom { name, neg } => {
            format!("[\"atom\", \"{}\", {}]", name, neg)
        }
        Guard::And(a, b) => format!("[\"and\", {}, {}]", py_guard(a), py_guard(b)),
        Guard::Or(a, b) => format!("[\"or\", {}, {}]", py_guard(a), py_guard(b)),
        // §38.3: expression guards serialize as a nested expression node
        Guard::ExprGuard(e) => format!("[\"expr\", {}]", py_expr(e)),
    }
}

fn py_opt_guard(g: &Option<Guard>) -> String {
    match g {
        Some(g) => py_guard(g),
        None => "None".into(),
    }
}

fn py_action(a: &ProtocolAction) -> String {
    match a {
        ProtocolAction::Send { performative, to, fields, .. } => format!(
            "{{\"kind\": \"send\", \"performative\": \"{}\", \"to\": {}, \"fields\": {}}}",
            py_perf(performative),
            match to {
                Some(t) => format!("\"{t}\""),
                None => "None".to_string(),
            },
            match fields {
                Some(f) => serde_json::to_string(f).unwrap_or_default(),
                None => "None".into(),
            }
        ),
        ProtocolAction::Enter { phase } => format!(
            "{{\"kind\": \"enter\", \"phase\": \"{phase}\"}}",
        ),
        ProtocolAction::BeginId { id } => format!(
            "{{\"kind\": \"begin_id\", \"id\": \"{id}\"}}",
        ),
        ProtocolAction::FulfilId { id } => format!(
            "{{\"kind\": \"fulfil_id\", \"id\": \"{id}\"}}",
        ),
        ProtocolAction::Try { body, catch } => format!(
            "{{\"kind\": \"try\", \"body\": [{}], \"catch\": [{}]}}",
            body.iter().map(py_action).collect::<Vec<_>>().join(", "),
            catch.iter().map(py_action).collect::<Vec<_>>().join(", ")
        ),
        ProtocolAction::Await { count, performative, into, within_ms, flag } => format!(
            "{{\"kind\": \"await\", \"count\": {count}, \"performative\": \"{}\", \"into\": \"{into}\", \"within_ms\": {within_ms}, \"flag\": \"{flag}\"}}",
            py_perf(performative)
        ),
        ProtocolAction::Broadcast { performative, fields, to } => format!(
            "{{\"kind\": \"broadcast\", \"performative\": \"{}\", \"to\": \"{}\", \"fields\": {}}}",
            py_perf(performative),
            to,
            match fields {
                Some(f) => serde_json::to_string(f).unwrap_or_default(),
                None => "None".into(),
            }
        ),
        ProtocolAction::Begin => "{\"kind\": \"begin\"}".into(),
        ProtocolAction::Fulfil => "{\"kind\": \"fulfil\"}".into(),
        ProtocolAction::Retract { target } => {
            format!("{{\"kind\": \"retract\", \"target\": \"{}\"}}", target)
        }
        ProtocolAction::Set { name, field, expr } => format!(
            "{{\"kind\": \"set\", \"name\": \"{}\", \"field\": {}, \"expr\": {}}}",
            name,
            match field { Some(f) => format!("\"{f}\""), None => "None".into() },
            py_expr(expr),
        ),
        ProtocolAction::Push { state, expr } => format!(
            "{{\"kind\": \"push\", \"state\": \"{}\", \"expr\": {}}}", state, py_expr(expr)
        ),
        ProtocolAction::Put { state, key, expr } => format!(
            "{{\"kind\": \"put\", \"state\": \"{}\", \"key\": \"{}\", \"expr\": {}}}", state, key, py_expr(expr)
        ),
        ProtocolAction::For { var, over, actions } => {
            let inner: Vec<String> = actions.iter().map(py_action).collect();
            format!(
                "{{\"kind\": \"for\", \"var\": \"{}\", \"over\": \"{}\", \"actions\": [{}]}}",
                var, over, inner.join(", ")
            )
        }
        ProtocolAction::Abort => "{\"kind\": \"abort\"}".into(),
        ProtocolAction::Return { expr } => format!(
            "{{\"kind\": \"return\", \"expr\": {}}}",
            py_expr(expr)
        ),
    }
}

/// §38.9: split a template text into literal/expr segments at `${...}`
/// boundaries (brace-balanced); `$${` is the literal escape.
enum FmtSeg {
    Text(String),
    Expr(Expr),
}

fn fmt_segments(txt: &str) -> Vec<FmtSeg> {
    let esc = "\u{1}";
    let cs: Vec<char> = txt.replace("$${", esc).chars().collect();
    let mut out = Vec::new();
    let mut lit = String::new();
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
                lit.push(cs[i]);
                i += 1;
                continue;
            }
            if !lit.is_empty() {
                let t = std::mem::take(&mut lit).replace(esc, "${");
                out.push(FmtSeg::Text(t));
            }
            let src: String = cs[i + 2..j - 1].iter().collect();
            match Expr::parse_expr(src.trim()) {
                Ok(e) => out.push(FmtSeg::Expr(e)),
                Err(_) => {
                    let t: String = cs[i..j].iter().collect();
                    out.push(FmtSeg::Text(t.replace(esc, "${")));
                }
            }
            i = j;
        } else {
            lit.push(cs[i]);
            i += 1;
        }
    }
    if !lit.is_empty() {
        out.push(FmtSeg::Text(lit.replace(esc, "${")));
    }
    // escape restoration inside expr-preceding texts is handled by the
    // final replacement below for already-pushed segments too
    out
}

fn py_expr(e: &Expr) -> String {
    match e {
        Expr::LitInt(i) => format!("{{\"lit\": {}}}", i),
        Expr::LitStr(s) => format!("{{\"lit\": {}}}", serde_json::to_string(s).unwrap_or_default()),
        Expr::LitBool(b) => format!("{{\"lit\": {}}}", b),
        Expr::State(n) => format!("{{\"state\": \"{}\"}}", n),
        // §38.3: capture references serialize as a capture node
        Expr::Capture(n) => format!("{{\"capture\": \"{}\"}}", n),
        Expr::Field { base, name } => format!(
            "{{\"field\": {{\"base\": {}, \"name\": \"{}\"}}}}",
            py_expr(base), name
        ),
        Expr::RecLit { ty, fields } => {
            let f: Vec<String> = fields.iter().map(|(n, e)| format!("\"{}\": {}", n, py_expr(e))).collect();
            format!("{{\"rec\": \"{}\", \"fields\": {{{}}}}}", ty, f.join(", "))
        }
        Expr::Arg(a) => format!("{{\"arg\": \"{}\"}}", a),
        Expr::Bin { op, l, r } => {
            let o = match op {
                BinOp::Add => "add", BinOp::Sub => "sub", BinOp::Mul => "mul", BinOp::Div => "div",
                BinOp::Lt => "lt", BinOp::Le => "le", BinOp::Gt => "gt", BinOp::Ge => "ge",
                BinOp::Eq => "eq", BinOp::Ne => "ne", BinOp::And => "and", BinOp::Or => "or",
            };
            format!("{{\"op\": \"{}\", \"l\": {}, \"r\": {}}}", o, py_expr(l), py_expr(r))
        }
        Expr::Not(x) => format!("{{\"not\": {}}}", py_expr(x)),
        Expr::Call { name, args } if name == "fmt" && !args.is_empty() => {
            // §38.9: fmt with an inline "${...}" literal is emitted as
            // pre-parsed segments; fmt with a template name keeps the
            // positional form (resolved against TEMPLATES at runtime)
            if let Expr::LitStr(txt) = &args[0] {
                if txt.contains("${") {
                    let segs = fmt_segments(txt);
                    let segs = segs
                        .iter()
                        .map(|seg| match seg {
                            FmtSeg::Text(t) => format!("{{\"s\": {}}}", serde_json::to_string(t).unwrap_or_default()),
                            FmtSeg::Expr(e) => format!("{{\"e\": {}}}", py_expr(e)),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    return format!("{{\"tmpl\": [{segs}]}}");
                }
            }
            let a: Vec<String> = args.iter().map(py_expr).collect();
            format!("{{\"fmtname\": {}, \"args\": [{}]}}", py_expr(&args[0]), a[1..].join(", "))
        }
        Expr::Call { name, args } => {
            let a: Vec<String> = args.iter().map(py_expr).collect();
            format!("{{\"call\": \"{}\", \"args\": [{}]}}", name, a.join(", "))
        }
    }
}

fn py_trigger(t: &OnTrigger) -> String {
    match t {
        OnTrigger::Event { performative, pattern, from, phase, .. } => format!(
            "{{\"kind\": \"event\", \"performative\": \"{}\", \"pattern\": {}, \"from\": {}, {{\"phase\": {}}}",
            py_perf(performative),
            match pattern {
                Some(p) => serde_json::to_string(p).unwrap_or_default(),
                None => "None".into(),
            },
            match from {
                Some(f) => format!("\"{f}\""),
                None => "None".to_string(),
            },
            match phase {
                Some(ph) => format!("\"{ph}\""),
                None => "None".to_string(),
            }
        ),
        OnTrigger::Breach => "{\"kind\": \"breach\"}".into(),
        OnTrigger::Expired => "{\"kind\": \"expired\"}".into(),
        OnTrigger::Timer { every_ms, guard, as_role } => format!(
            "{{\"kind\": \"timer\", \"every_ms\": {}, \"guard\": {}, \"as_role\": {}}}",
            every_ms,
            py_opt_guard(guard),
            match as_role { Some(r) => format!("\"{r}\""), None => "None".into() }
        ),
    }
}

/// Generate the Python agent skeleton for one role's endpoint.
pub fn gen_agent_py(prog: &ProtocolProgram, role: &str) -> Result<String, Vec<crate::idl::IdlError>> {
    let ep: RoleEndpoint = project(prog, role)?;
    let mut out = String::new();
    out.push_str(&format!(
        "\"\"\"{} agent (role: {}) — generated from protocol '{}' v{} by aclc.\n\n\
        The conversation is the program (ACL spec §33/§34): this class is the\n\
        compiled protocol, not hand-written logic. Wire it to any transport\n\
        by feeding deliver()/tick() and sending the returned dicts.\n\"\"\"\n\n",
        prog.name, role, prog.name, prog.version
    ));
    out.push_str("class Agent:\n");
    out.push_str(&format!("    ROLE = {}\n", serde_json::to_string(role).unwrap_or_default()));
    out.push_str(&format!(
        "    ROLES = {}\n",
        serde_json::to_string(&prog.roles).unwrap_or_default()
    ));
    out.push_str(&format!(
        "    STATES = {}\n",
        serde_json::to_string(
            &prog.states
                .iter()
                .map(|s| (s.name.clone(), s.ty.clone().unwrap_or_else(|| "bool".into())))
                .collect::<std::collections::BTreeMap<_, _>>()
        )
        .unwrap_or_default()
    ));
    // §38.9: templates are pre-parsed into literal/expr segments
    let templates: std::collections::BTreeMap<String, serde_json::Value> = prog
        .templates
        .iter()
        .map(|(n, t)| {
            let segs: Vec<serde_json::Value> = fmt_segments(t)
                .into_iter()
                .map(|seg| match seg {
                    FmtSeg::Text(t) => serde_json::json!({ "s": t }),
                    FmtSeg::Expr(e) => serde_json::json!({
                        "e": serde_json::from_str::<serde_json::Value>(&py_expr(&e)).unwrap_or_default()
                    }),
                })
                .collect();
            (n.clone(), serde_json::json!(segs))
        })
        .collect();
    out.push_str(&format!(
        "    PHASES = {}\n",
        serde_json::to_string(&prog.phases).unwrap_or_default()
    ));
    out.push_str(&format!(
        "    TEMPLATES = {}\n",
        serde_json::to_string(&templates).unwrap_or_default()
    ));
    out.push_str("    HANDLERS = [\n");
    for h in &ep.handlers {
        let (kind, base) = match &h.trigger {
            OnTrigger::Event { performative, from, phase, .. } => {
                ("event", format!("\"performative\": \"{}\", \"from\": {}, \"phase\": {}", py_perf(performative), match from { Some(f) => format!("\"{f}\""), None => "None".into() }, match phase { Some(ph) => format!("\"{ph}\""), None => "None".into() }))
            }
            OnTrigger::Breach => ("breach", String::new()),
            OnTrigger::Expired => ("expired", String::new()),
            OnTrigger::Timer { every_ms, guard, as_role } => ("timer", format!("\"every_ms\": {}, \"as_role\": {}", every_ms, match as_role { Some(r) => format!("\"{r}\""), None => "None".into() })),
        };
        let guard = match &h.trigger {
            OnTrigger::Timer { guard, .. } => py_opt_guard(guard),
            _ => py_opt_guard(&h.guard),
        };
        let pattern = match &h.trigger {
            OnTrigger::Event { pattern, .. } => match pattern {
                Some(p) => serde_json::to_string(p).unwrap_or_default(),
                None => "None".into(),
            },
            _ => "None".into(),
        };
        let actions: Vec<String> = h.actions.iter().map(py_action).collect();
        let mut parts = vec![format!("\"kind\": \"{}\"", kind)];
        if !base.is_empty() {
            parts.push(base);
        }
        parts.push(format!("\"pattern\": {}", pattern));
        parts.push(format!("\"guard\": {}", guard));
        parts.push(format!("\"actions\": [{}]", actions.join(", ")));
        out.push_str(&format!("        {{{}}},\n", parts.join(", ")));
    }
    out.push_str("    ]\n\n");

    // the runtime loop — mirrors src/runtime.rs semantics exactly
    out.push_str(
        r#"    def __init__(self):
        self._arg = {}
        self._caps = {}
        self._awaits = []
        self._catch = None
        self._jump = False
        self._phase = self.PHASES[0] if self.PHASES else ""
        self.commitments = {}
        self.states = {k: None for k in self.STATES}
        self.commitment = "proposed"   # proposed -> active -> fulfilled/breached
        self.timer_due = [0] * len([h for h in self.HANDLERS if h["kind"] == "timer"])
        self._now = 0
        self.transcript = []

    # -- state helpers ------------------------------------------------
    def _bool(self, name):
        v = self.states.get(name)
        if v is None:
            return False
        if isinstance(v, bool):
            return v
        if isinstance(v, int):
            return v != 0
        return len(v) > 0

    def _text(self, v):
        if v is True:
            return "true"
        if v is False:
            return "false"
        if v is None:
            return "unset"
        return str(v)

    def _guard(self, g):
        if g is None:
            return True
        kind = g[0]
        if kind == "atom":
            v = self._bool(g[1])
            return (not v) if g[2] else v
        if kind == "expr":
            return self._truthy(self._eval(g[1]))
        if kind == "and":
            return self._guard(g[1]) and self._guard(g[2])
        return self._guard(g[1]) or self._guard(g[2])

    def _pattern_ok(self, want, got_pattern, got_fields):
        if want is None:
            return True
        if want.startswith("("):
            return want == got_pattern
        if got_fields is None:
            return False
        import json
        w = json.loads(want)
        self._caps = {}
        for k, wv in w.items():
            gv = got_fields.get(k)
            if gv is None:
                return False
            if wv.endswith("*"):
                if not str(gv).startswith(wv[:-1]):
                    return False
            elif wv.startswith("$"):
                # §46: native typed values pass through; strings infer
                self._caps[wv[1:]] = gv if isinstance(gv, (int, float, bool)) else self._infer(gv)
            elif str(gv) != wv:
                return False
        return True

    def _infer(self, s):
        # §38.3: capture type inference (int/float/bool, else string)
        if s == "true":
            return True
        if s == "false":
            return False
        try:
            return int(s)
        except ValueError:
            pass
        try:
            return float(s)
        except ValueError:
            return s

    def _match(self, h, performative, pattern, fields):
        if h["kind"] == "event":
            if h["performative"] != performative:
                return False
            # §50: conversation phase gate
            if h.get("phase") is not None and self._phase != h["phase"]:
                return False
            if not self._pattern_ok(h["pattern"], pattern, fields):
                return False
            return self._guard(h["guard"])
        return False

    def _eval(self, e):
        if "tmpl" in e:
            # §38.9 rich interpolation: pre-parsed segments
            out = ""
            for seg in e["tmpl"]:
                if "s" in seg:
                    out += seg["s"]
                else:
                    out += self._text(self._eval(seg["e"]))
            return out
        if "fmtname" in e:
            tname = self._eval(e["fmtname"])
            segs = self.TEMPLATES.get(tname)
            if segs is None:
                raise ValueError("no message template '%s'" % tname)
            out = ""
            for seg in segs:
                if "s" in seg:
                    out += seg["s"]
                else:
                    out += self._text(self._eval(seg["e"]))
            for idx, a in enumerate(e["args"]):
                out = out.replace("{%d}" % idx, self._text(self._eval(a)))
            return out
        if "lit" in e:
            return e["lit"]
        if "state" in e:
            return self.states.get(e["state"])
        if "capture" in e:
            return self._caps.get(e["capture"])
        if "arg" in e:
            return self._arg.get(e["arg"])
        if "not" in e:
            return not self._truthy(self._eval(e["not"]))
        if "op" in e:
            l = self._eval(e["l"]); r = self._eval(e["r"])
            op = e["op"]
            if op == "and": return self._truthy(l) and self._truthy(r)
            if op == "or": return self._truthy(l) or self._truthy(r)
            # unset states behave as 0 in arithmetic (§35.1 convention)
            if op == "add": return (l or 0) + (r or 0)
            if op == "sub": return (l or 0) - (r or 0)
            if op == "mul": return (l or 0) * (r or 0)
            if op == "div": return (l or 0) // (r or 0)
            if op == "eq": return l == r
            if op == "ne": return l != r
            if op == "lt": return l < r
            if op == "le": return l <= r
            if op == "gt": return l > r
            if op == "ge": return l >= r
        if "call" in e:
            args = [self._eval(a) for a in e["args"]]
            name = e["call"]
            if name == "add": return (args[0] or 0) + (args[1] or 0)
            if name == "sub": return (args[0] or 0) - (args[1] or 0)
            if name == "mul": return (args[0] or 0) * (args[1] or 0)
            if name == "div": return (args[0] or 0) // (args[1] or 0)
            if name == "min": return min(args)
            if name == "max": return max(args)
            if name == "abs": return abs(args[0])
            if name == "len": return len(args[0])
            if name == "sum": return sum(args[0])
            if name == "count": return len(args[0])
            if name == "concat": return str(args[0]) + str(args[1])
            if name == "upper": return str(args[0]).upper()
            if name == "lower": return str(args[0]).lower()
            if name == "str": return str(args[0])
            if name == "int": return int(args[0])
            if name == "exists": return args[0] is not None
            if name == "prefer":
                # §38.8: lowest value of args[1] among maps in args[0]
                best = None; best_v = None
                for m in (args[0] or []):
                    v = (m or {}).get(args[1])
                    if isinstance(v, (int, float)) and (best_v is None or v < best_v):
                        best, best_v = m, v
                return best
            if name == "first": return args[0][0]
            if name == "last": return args[0][-1]
            if name == "fmt":
                raise NotImplementedError("fmt requires template support")
        raise ValueError("bad expression: %r" % e)

    def _truthy(self, v):
        if v is None: return False
        if isinstance(v, bool): return v
        if isinstance(v, (int, float)): return v != 0
        if isinstance(v, (str, list, dict)): return len(v) > 0
        return False

    def _run(self, h):
        out = []
        for a in h["actions"]:
            if a["kind"] == "send":
                self.transcript.append(("out", a["performative"]))
                out.append({"to": a["to"], "performative": a["performative"], "fields": a["fields"]})
            elif a["kind"] == "begin":
                if self.commitment == "proposed":
                    self.commitment = "active"
                self.transcript.append(("state", "begin"))
            elif a["kind"] == "fulfil":
                if self.commitment == "active":
                    self.commitment = "fulfilled"
                self.transcript.append(("state", "fulfil"))
            elif a["kind"] == "retract":
                self.states[a["target"]] = False
                self.transcript.append(("state", "retract:" + a["target"]))
            elif a["kind"] == "set":
                self.states[a["name"]] = self._eval(a["expr"])
                self.transcript.append(("state", "set:" + a["name"]))
            elif a["kind"] == "try":
                # §45: handler-local error flow — a failing action in
                # the body cuts it short and runs the catch actions
                self._catch = a["catch"]
                for b in a["body"]:
                    self._run_one(b, out)
                    if self._jump:
                        break
                self._catch = None
                self._jump = False
            elif a["kind"] == "begin_id":
                # §51: named commitment lifecycle
                cur = self.commitments.get(a["id"])
                if cur in ("proposed", "active"):
                    self.transcript.append(("state", "begin " + a["id"] + " ignored"))
                else:
                    self.commitments[a["id"]] = "active"
                    self.transcript.append(("state", "commitment " + a["id"] + " -> active"))
            elif a["kind"] == "fulfil_id":
                if self.commitments.get(a["id"]) == "active":
                    self.commitments[a["id"]] = "fulfilled"
                    self.transcript.append(("state", "commitment " + a["id"] + " -> fulfilled"))
                else:
                    self.transcript.append(("state", "fulfil " + a["id"] + " ignored"))
            elif a["kind"] == "enter":
                # §50: conversation phase transition
                self._phase = a["phase"]
                self.transcript.append(("state", "phase:" + a["phase"]))
            elif a["kind"] == "await":
                # §44: arm the collector; flag starts False
                self._awaits.append({
                    "count_left": a["count"],
                    "performative": a["performative"],
                    "into": a["into"],
                    "deadline_ms": self._now + a["within_ms"],
                    "flag": a["flag"],
                })
                self.states[a["flag"]] = False
            elif a["kind"] == "broadcast":
                # §38.8: fan-out — one send per role in the list state
                for r in (self.states.get(a["to"]) or []):
                    self.transcript.append(("out", a["performative"]))
                    out.append({"to": r, "performative": a["performative"], "fields": a["fields"]})
            elif a["kind"] == "push":
                cur = self.states.get(a["state"])
                if cur is None:
                    self.states[a["state"]] = [self._eval(a["expr"])]
                elif isinstance(cur, list):
                    cur.append(self._eval(a["expr"]))
                self.transcript.append(("state", "push:" + a["state"]))
            elif a["kind"] == "put":
                cur = self.states.get(a["state"])
                if cur is None:
                    self.states[a["state"]] = {}
                if isinstance(self.states[a["state"]], dict):
                    key = self._eval({"lit": 0, "str": a["key"]}) if False else a["key"]
                    self.states[a["state"]][key] = self._eval(a["expr"])
                self.transcript.append(("state", "put:" + a["state"]))
            elif a["kind"] == "for":
                lst = list(self.states.get(a["over"]) or [])
                saved = self.states.get(a["var"])
                for item in lst:
                    self.states[a["var"]] = item
                    for sub in a["actions"]:
                        self._exec_one(sub)
                if saved is None:
                    self.states.pop(a["var"], None)
                else:
                    self.states[a["var"]] = saved
            elif a["kind"] == "abort":
                self.commitment = "breached"
                self.transcript.append(("state", "abort"))
        return out

    def _exec_one(self, a):
        self._run({"actions": [a]})

    # -- public surface -----------------------------------------------
    def deliver(self, from_role, performative, pattern=None, fields=None):
        """Feed one incoming speech act; returns the outgoing ones."""
        self._arg = fields or {}
        # §44: an active await collector CONSUMES matching events
        for aw in list(self._awaits):
            if aw["performative"] == performative:
                payload = dict(fields or {})
                cur = self.states.get(aw["into"])
                if isinstance(cur, list):
                    cur.append(payload)
                else:
                    self.states[aw["into"]] = [payload]
                aw["count_left"] -= 1
                if aw["count_left"] <= 0:
                    self._awaits.remove(aw)
                    self.states[aw["flag"]] = True
                return []
        self.transcript.append(("in", performative, from_role))
        for h in self.HANDLERS:
            if h["kind"] == "breach" or h["kind"] == "expired":
                continue
            if h["kind"] == "event" and h["from"] is not None and h["from"] != from_role:
                continue
            if self._match(h, performative, pattern, fields):
                return self._run(h)
        return []   # logged, unhandled (§33.4 auditability)

    def _run_one(self, a, out):
        # §45: one action; failures route like the Rust runtime
        try:
            if a["kind"] == "send":
                self.transcript.append(("out", a["performative"]))
                out.append({"to": a["to"], "performative": a["performative"], "fields": a["fields"]})
            elif a["kind"] == "begin":
                if self.commitment == "proposed":
                    self.commitment = "active"
                self.transcript.append(("state", "begin"))
            elif a["kind"] == "fulfil":
                if self.commitment == "active":
                    self.commitment = "fulfilled"
                self.transcript.append(("state", "fulfil"))
            elif a["kind"] == "retract":
                self.states[a["target"]] = False
                self.transcript.append(("state", "retract:" + a["target"]))
            elif a["kind"] == "set":
                self.states[a["name"]] = self._eval(a["expr"])
                self.transcript.append(("state", "set:" + a["name"]))
            elif a["kind"] == "try":
                # §45: handler-local error flow — a failing action in
                # the body cuts it short and runs the catch actions
                self._catch = a["catch"]
                for b in a["body"]:
                    self._run_one(b, out)
                    if self._jump:
                        break
                self._catch = None
                self._jump = False
            elif a["kind"] == "await":
                # §44: arm the collector; flag starts False
                self._awaits.append({
                    "count_left": a["count"],
                    "performative": a["performative"],
                    "into": a["into"],
                    "deadline_ms": self._now + a["within_ms"],
                    "flag": a["flag"],
                })
                self.states[a["flag"]] = False
            elif a["kind"] == "broadcast":
                # §38.8: fan-out — one send per role in the list state
                for r in (self.states.get(a["to"]) or []):
                    self.transcript.append(("out", a["performative"]))
                    out.append({"to": r, "performative": a["performative"], "fields": a["fields"]})
            elif a["kind"] == "push":
                cur = self.states.get(a["state"])
                if cur is None:
                    self.states[a["state"]] = [self._eval(a["expr"])]
                elif isinstance(cur, list):
                    cur.append(self._eval(a["expr"]))
                self.transcript.append(("state", "push:" + a["state"]))
            elif a["kind"] == "put":
                cur = self.states.get(a["state"])
                if cur is None:
                    self.states[a["state"]] = {}
                if isinstance(self.states[a["state"]], dict):
                    key = self._eval({"lit": 0, "str": a["key"]}) if False else a["key"]
                    self.states[a["state"]][key] = self._eval(a["expr"])
                self.transcript.append(("state", "put:" + a["state"]))
            elif a["kind"] == "for":
                lst = list(self.states.get(a["over"]) or [])
                saved = self.states.get(a["var"])
                for item in lst:
                    self.states[a["var"]] = item
                    for sub in a["actions"]:
                        self._exec_one(sub)
                if saved is None:
                    self.states.pop(a["var"], None)
                else:
                    self.states[a["var"]] = saved
            elif a["kind"] == "abort":
                self.commitment = "breached"
                self.transcript.append(("state", "abort"))
        except Exception as e:
            if self._catch:
                catch = self._catch
                self._catch = None
                for c in catch:
                    self._run_one(c, out)
                self._jump = True
            else:
                self.transcript.append(("state", "internal error: %s" % e))

    def fire(self, kind):
        """Deliver 'breach' or 'expired' state events."""
        self.transcript.append(("in", kind))
        for h in self.HANDLERS:
            if h["kind"] == kind:
                return self._run(h)
        return []

    def tick(self, elapsed_ms):
        """Host-injected time (§34): timers fire here, deterministically."""
        self._now = elapsed_ms
        # §44: await deadlines
        for aw in list(self._awaits):
            if aw["deadline_ms"] <= elapsed_ms:
                self._awaits.remove(aw)
                self.states[aw["flag"]] = False
        timers = [h for h in self.HANDLERS if h["kind"] == "timer"]
        out = []
        for i, h in enumerate(timers):
            if not self._guard(h["guard"]):
                continue
            while self.timer_due[i] + h["every_ms"] <= elapsed_ms:
                self.timer_due[i] += h["every_ms"]
                self.transcript.append(("in", "timer"))
                out.extend(self._run(h))
        return out

    def ended(self):
        if "fulfilled" == self.commitment:
            return True
        return False
"#,
    );
    Ok(out)
}
