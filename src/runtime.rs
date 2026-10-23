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
use crate::protocol::{EndWhen, Handler, OnTrigger, ProtocolAction, ProtocolProgram};
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
fn handler_executor<'a>(prog: &'a ProtocolProgram, h: &Handler) -> Vec<&'a str> {
    match &h.trigger {
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
    if prog.roles.len() != 2 {
        errors.push(IdlError {
            line: 0,
            message: format!(
                "projection supports exactly 2 roles, protocol declares {}",
                prog.roles.len()
            ),
        });
    }

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
                ProtocolAction::Send { performative, to } => {
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
                        None => Some(other.to_string()),
                    };
                    if let Some(t) = target {
                        if !t.is_empty() {
                            // §33.5: the receiving role sees this send as an
                            // incoming event — nothing to do in ITS handlers;
                            // the executor emits it
                            actions.push(ProtocolAction::Send {
                                performative: *performative,
                                to: Some(t),
                            });
                        }
                    }
                }
                other_action => actions.push(other_action.clone()),
            }
        }
        if !actions.is_empty() || matches!(h.trigger, OnTrigger::Event { .. }) {
            handlers.push(Handler { trigger: h.trigger.clone(), actions });
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
    Bool(bool),
    Int(i64),
    Str(String),
    Unset,
}

/// Something that happens TO the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// an incoming speech act from another role
    In { from: String, performative: Performative, pattern: Option<String> },
    /// an owed commitment was broken (by the OTHER side or by time)
    Breach,
    /// the deadline passed
    Expired,
}

/// Something the agent emits.
#[derive(Debug, Clone, PartialEq)]
pub struct OutEvent {
    pub to: Option<String>,
    pub performative: Performative,
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
    /// this role's projected view — the runtime NEVER sees handlers
    /// that belong to another role (§33.5)
    endpoint: RoleEndpoint,
    end_when: Option<EndWhen>,
    states: BTreeMap<String, StateValue>,
    commitment: CommitmentStatus,
    log: Vec<LogEntry>,
    seq: u64,
}

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
            states: prog
                .states
                .iter()
                .map(|s| (s.name.clone(), StateValue::Unset))
                .collect(),
            commitment: CommitmentStatus::Proposed,
            log: Vec::new(),
            seq: 0,
        })
    }

    pub fn state(&self, name: &str) -> Option<&StateValue> {
        self.states.get(name)
    }

    pub fn set_state(&mut self, name: &str, v: StateValue) {
        self.states.insert(name.to_string(), v);
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

    /// Deliver an event: find the FIRST handler whose trigger matches and
    /// execute its actions in order. Returns what the agent emits.
    pub fn deliver(&mut self, ev: Event) -> Vec<OutEvent> {
        // every incoming speech act enters the transcript, handled or not
        self.log(Dir::In, event_text(&ev));
        match self.matching_handler(&ev) {
            Some(hidx) => self.run_handler(hidx, &ev),
            None => Vec::new(),
        }
    }

    fn matching_handler(&self, ev: &Event) -> Option<usize> {
        self.endpoint.handlers.iter().position(|h| match (&h.trigger, ev) {
            (OnTrigger::Event { performative, pattern, from }, Event::In { from: f, performative: p, pattern: pat }) => {
                performative == p
                    && pattern.as_ref().zip(pat.as_ref()).map_or(true, |(a, b)| a == b)
                    && from.as_ref().map_or(true, |want| want == f)
            }
            (OnTrigger::Breach, Event::Breach) => true,
            (OnTrigger::Expired, Event::Expired) => true,
            _ => false,
        })
    }

    fn run_handler(&mut self, hidx: usize, ev: &Event) -> Vec<OutEvent> {
        let actions = self.endpoint.handlers[hidx].actions.clone();
        let mut out = Vec::new();
        for a in &actions {
            match a {
                ProtocolAction::Send { performative, to } => {
                    self.log(
                        Dir::Out,
                        format!("send {} to {}", perf_name(*performative), to.clone().unwrap_or_default()),
                    );
                    out.push(OutEvent { to: to.clone(), performative: *performative });
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
                    self.set_state(target, StateValue::Bool(false));
                    self.log(Dir::State, format!("retract({target})"));
                }
            }
        }
        let _ = ev;
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
        Event::In { from, performative, pattern } => format!(
            "recv {} from {}{}",
            perf_name(*performative),
            from,
            pattern.clone().unwrap_or_default()
        ),
        Event::Breach => "breach".into(),
        Event::Expired => "expired".into(),
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
end when fulfilled;
"#;

    #[test]
    fn projection_gives_each_role_only_its_side() {
        let prog = parse(SUPPLY).unwrap();
        let buyer = project(&prog, "buyer").unwrap();
        let seller = project(&prog, "seller").unwrap();

        // buyer is the INITIATOR: every trigger is attributed `from buyer`
        // or fires in the debtor (seller) — so buyer's endpoint has NO
        // handlers. buyer acts, it does not react. (§33.5)
        assert!(buyer.handlers.is_empty(), "buyer endpoint should be empty, got {:?}", buyer.handlers);

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
            ProtocolAction::Send { performative: Performative::Propose, to: Some("buyer".into()) }
        );
        // pretty roundtrip of the extended grammar still works
        assert!(parse(&crate::protocol::pretty(&prog)).is_ok());
    }

    #[test]
    fn projection_rejects_unknown_role_and_n3() {
        let prog = parse(SUPPLY).unwrap();
        assert!(project(&prog, "intruder").is_err());
        let three = parse(
            "protocol t v1;\nroles a, b, c;\non query from a -> send inform to a;\n",
        )
        .unwrap();
        let errs = project(&three, "b").unwrap_err();
        assert!(errs[0].message.contains("exactly 2 roles"));
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
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].performative, Performative::Propose);
        assert_eq!(out[0].to.as_deref(), Some("buyer"));

        // accept arrives -> begin: commitment proposed -> active
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Accept,
            pattern: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Active);

        // paid arrives -> fulfil
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Inform,
            pattern: Some("(\"paid\")".into()),
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
        });
        assert_eq!(out[0].performative, Performative::Propose);
        assert_eq!(out[0].to.as_deref(), Some("buyer"));

        // propose arrives at buyer: buyer's endpoint has no handlers, but
        // the event enters ITS transcript (the wire is observable)
        let back = buyer.deliver(Event::In {
            from: "seller".into(),
            performative: Performative::Propose,
            pattern: None,
        });
        assert!(back.is_empty());

        // buyer accepts -> seller's commitment activates
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Accept,
            pattern: None,
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Active);

        // buyer pays -> fulfil -> session ends
        seller.deliver(Event::In {
            from: "buyer".into(),
            performative: Performative::Inform,
            pattern: Some("(\"paid\")".into()),
        });
        assert_eq!(seller.commitment(), CommitmentStatus::Fulfilled);
        assert!(seller.ended());

        // the two transcripts together ARE the global conversation
        let combined: Vec<&LogEntry> = buyer
            .transcript()
            .iter()
            .chain(seller.transcript().iter())
            .collect();
        assert_eq!(combined.iter().filter(|e| e.dir == Dir::In).count(), 4);
        assert_eq!(combined.iter().filter(|e| e.dir == Dir::Out).count(), 1);
        assert!(ep_buyer.handlers.is_empty());
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
        });
        assert_eq!(a.state("flag"), Some(&StateValue::Bool(false)));
        assert!(a.transcript().iter().any(|e| e.text == "retract(flag)"));
    }

    #[test]
    fn begin_twice_is_ignored_not_fatal() {
        let prog = parse("protocol p v1;\nroles a, b;\non inform from b -> begin;\n").unwrap();
        let mut a = Runtime::new(&prog, "a").unwrap();
        a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None });
        assert_eq!(a.commitment(), CommitmentStatus::Active);
        a.deliver(Event::In { from: "b".into(), performative: Performative::Inform, pattern: None });
        assert_eq!(a.commitment(), CommitmentStatus::Active);
        assert!(a.transcript().iter().any(|e| e.text.contains("begin ignored")));
    }
}
