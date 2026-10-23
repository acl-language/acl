//! §34: endpoint codegen — `aclc gen agent py <file.aclp> <out_dir>`.
//!
//! Emits a RUNNABLE Python skeleton per role: the projected handlers as
//! data, the dispatch loop, guard evaluation, and timer ticking. The
//! generated agent has no ACL-specific dependencies — it speaks the
//! protocol the program declares, exactly as compiled.

use crate::protocol::{Guard, OnTrigger, ProtocolAction, ProtocolProgram};
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
        ProtocolAction::Send { performative, to, fields } => format!(
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
        ProtocolAction::Begin => "{\"kind\": \"begin\"}".into(),
        ProtocolAction::Fulfil => "{\"kind\": \"fulfil\"}".into(),
        ProtocolAction::Retract { target } => {
            format!("{{\"kind\": \"retract\", \"target\": \"{}\"}}", target)
        }
    }
}

fn py_trigger(t: &OnTrigger) -> String {
    match t {
        OnTrigger::Event { performative, pattern, from } => format!(
            "{{\"kind\": \"event\", \"performative\": \"{}\", \"pattern\": {}, \"from\": {}}}",
            py_perf(performative),
            match pattern {
                Some(p) => serde_json::to_string(p).unwrap_or_default(),
                None => "None".into(),
            },
            match from {
                Some(f) => format!("\"{f}\""),
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
    out.push_str("    HANDLERS = [\n");
    for h in &ep.handlers {
        let (kind, base) = match &h.trigger {
            OnTrigger::Event { performative, from, .. } => {
                ("event", format!("\"performative\": \"{}\", \"from\": {}", py_perf(performative), match from { Some(f) => format!("\"{f}\""), None => "None".into() }))
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
        self.states = {k: None for k in self.STATES}
        self.commitment = "proposed"   # proposed -> active -> fulfilled/breached
        self.timer_due = [0] * len([h for h in self.HANDLERS if h["kind"] == "timer"])
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

    def _guard(self, g):
        if g is None:
            return True
        kind = g[0]
        if kind == "atom":
            v = self._bool(g[1])
            return (not v) if g[2] else v
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
        for k, wv in w.items():
            gv = got_fields.get(k)
            if gv is None:
                return False
            if wv.endswith("*"):
                if not gv.startswith(wv[:-1]):
                    return False
            elif gv != wv:
                return False
        return True

    def _match(self, h, performative, pattern, fields):
        if h["kind"] == "event":
            if h["performative"] != performative:
                return False
            if not self._pattern_ok(h["pattern"], pattern, fields):
                return False
            return self._guard(h["guard"])
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
        return out

    # -- public surface -----------------------------------------------
    def deliver(self, from_role, performative, pattern=None, fields=None):
        """Feed one incoming speech act; returns the outgoing ones."""
        self.transcript.append(("in", performative, from_role))
        for h in self.HANDLERS:
            if h["kind"] == "breach" or h["kind"] == "expired":
                continue
            if h["kind"] == "event" and h["from"] is not None and h["from"] != from_role:
                continue
            if self._match(h, performative, pattern, fields):
                return self._run(h)
        return []   # logged, unhandled (§33.4 auditability)

    def fire(self, kind):
        """Deliver 'breach' or 'expired' state events."""
        self.transcript.append(("in", kind))
        for h in self.HANDLERS:
            if h["kind"] == kind:
                return self._run(h)
        return []

    def tick(self, elapsed_ms):
        """Host-injected time (§34): timers fire here, deterministically."""
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
