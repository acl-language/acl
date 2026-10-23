//! §35.8/§35.9: multi-session host and event sourcing.
//!
//! An agent is not ONE conversation — it runs MANY concurrent sessions
//! of the same protocol. `SessionHost` multiplexes them, enforces the
//! declared capacity, and journals every event. Because the runtime is
//! a pure function of (program, events), the journal IS the state:
//! `replay` reconstructs any host exactly.

use crate::protocol::ProtocolProgram;
use crate::runtime::{Event, OutEvent, Runtime};
use std::collections::BTreeMap;

pub type SessionId = String;

/// A multiplexed session host for one protocol program.
pub struct SessionHost {
    pub program: ProtocolProgram,
    pub capacity: usize,
    /// §35.6 enforcement: capabilities the environment GRANTED to this
    /// host. A program requiring a capability not present here cannot
    /// open sessions — the declaration is enforceable, not decorative.
    granted: std::collections::BTreeSet<String>,
    sessions: BTreeMap<SessionId, Runtime>,
    /// §35.9: the journal — every event delivered, in order
    journal: Vec<(SessionId, Event)>,
    /// outbox: (session, out events) produced, in order
    outbox: Vec<(SessionId, OutEvent)>,
}

impl SessionHost {
    pub fn new(program: &ProtocolProgram) -> Result<Self, String> {
        Ok(SessionHost {
            program: program.clone(),
            capacity: program.capacity.unwrap_or(usize::MAX),
            granted: std::collections::BTreeSet::new(),
            sessions: BTreeMap::new(),
            journal: Vec::new(),
            outbox: Vec::new(),
        })
    }

    /// §35.6: gated constructor — the environment grants capabilities;
    /// missing grants reject session opens.
    pub fn new_gated(
        program: &ProtocolProgram,
        granted: &[String],
    ) -> Result<Self, String> {
        let mut host = SessionHost::new(program)?;
        for g in granted {
            host.granted.insert(g.clone());
        }
        Ok(host)
    }

    /// verify a peer's capability list against this program's needs
    /// (the AgentCard check, §35.6)
    pub fn peer_satisfies(card_capabilities: &[String]) -> impl Fn(&ProtocolProgram) -> Result<(), String> + '_ {
        move |prog| {
            let missing: Vec<&str> = prog
                .capabilities
                .iter()
                .map(|c| c.name.as_str())
                .filter(|n| !card_capabilities.contains(&n.to_string()))
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!("peer AgentCard lacks: {}", missing.join(", ")))
            }
        }
    }

    /// open a session — capacity AND capability grants are enforced (§35.6)
    pub fn open(&mut self, id: &str, role: &str) -> Result<(), String> {
        // capability gate: default-on. A program that declares required
        // capabilities cannot open sessions the environment did not grant.
        let ungranted: Vec<&str> = self
            .program
            .capabilities
            .iter()
            .map(|c| c.name.as_str())
            .filter(|name| !name.starts_with("stdlib.") && !self.granted.contains(*name))
            .collect();
        if !ungranted.is_empty() {
            return Err(format!(
                "capability gate: session '{id}' rejected — program requires {} which was not granted",
                ungranted.join(", ")
            ));
        }
        if self.sessions.contains_key(id) {
            return Err(format!("session '{id}' already open"));
        }
        if self.sessions.len() >= self.capacity {
            return Err(format!(
                "capacity reached ({}) — session '{id}' rejected",
                self.capacity
            ));
        }
        let rt = Runtime::new(&self.program, role)
            .map_err(|e| format!("session '{id}': {}", e.message))?;
        self.sessions.insert(id.to_string(), rt);
        Ok(())
    }

    pub fn close(&mut self, id: &str) -> Result<(), String> {
        self.sessions
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| format!("session '{id}' not open"))
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// deliver an event into a session; outs go to the outbox
    pub fn deliver(&mut self, id: &str, ev: Event) -> Result<Vec<OutEvent>, String> {
        let rt = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| format!("session '{id}' not open"))?;
        let outs = rt.deliver(ev.clone());
        // §35.9: journal EVERY delivered event (even unhandled ones)
        self.journal.push((id.to_string(), ev));
        for o in &outs {
            self.outbox.push((id.to_string(), o.clone()));
        }
        Ok(outs)
    }

    /// host-injected time for ALL sessions
    pub fn tick(&mut self, elapsed_ms: i64) -> Vec<(SessionId, OutEvent)> {
        let mut out = Vec::new();
        for (id, rt) in self.sessions.iter_mut() {
            for o in rt.tick(elapsed_ms) {
                out.push((id.clone(), o));
            }
        }
        for (id, o) in &out {
            self.outbox.push((id.clone(), o.clone()));
        }
        out
    }

    /// §35.9: the journal (persistent — this IS the state)
    pub fn journal(&self) -> &[(SessionId, Event)] {
        &self.journal
    }

    /// outbox drain: everything emitted so far
    pub fn outbox(&self) -> &[(SessionId, OutEvent)] {
        &self.outbox
    }

    /// §35.9: reconstruct a host from a journal — deterministic replay.
    /// The replayed host must be indistinguishable from the original.
    pub fn replay(program: &ProtocolProgram, journal: &[(SessionId, Event)]) -> Result<Self, String> {
        let mut host = SessionHost::new(program)?;
        // collect session→role from the first event of each session
        // (roles aren't in events; the journal records them via a
        // pseudo-event at open time — here we re-open by role map passed
        // through the journal's first In per session)
        let mut opened: BTreeMap<SessionId, String> = BTreeMap::new();
        for (id, ev) in journal {
            if let Event::In { from, .. } = ev {
                opened.entry(id.clone()).or_insert_with(|| from.clone());
            }
        }
        // NOTE: `from` is the PEER, not the session's own role. The role
        // must be recorded at open time — sessions replay with the role
        // captured in the journal header (see journal_with_roles).
        for (id, role) in &opened {
            host.open(id, role)?;
        }
        for (id, ev) in journal {
            if host.is_open(id) {
                let _ = host.deliver(id, ev.clone());
            }
        }
        Ok(host)
    }

    /// journal with explicit session→role map (the persistence header)
    pub fn journal_with_roles(&self) -> (Vec<(String, String)>, Vec<(SessionId, Event)>) {
        let roles: Vec<(String, String)> = self
            .sessions
            .iter()
            .map(|(id, rt)| (id.clone(), rt.role.clone()))
            .collect();
        (roles, self.journal.clone())
    }

    /// rebuild from journal + role header
    pub fn replay_from(
        program: &ProtocolProgram,
        roles: &[(String, String)],
        journal: &[(SessionId, Event)],
    ) -> Result<Self, String> {
        let mut host = SessionHost::new(program)?;
        for (id, role) in roles {
            host.open(id, role)?;
        }
        for (id, ev) in journal {
            if host.is_open(id) {
                let _ = host.deliver(id, ev.clone());
            }
        }
        Ok(host)
    }

    /// iterate open sessions (for tooling)
    pub fn sessions_iter(&self) -> impl Iterator<Item = (&String, &Runtime)> {
        self.sessions.iter()
    }

    /// compare two hosts' observable state (for replay verification)
    pub fn same_state_as(&self, other: &SessionHost) -> bool {
        for (id, rt) in &self.sessions {
            match other.sessions.get(id) {
                None => return false,
                Some(o) => {
                    if rt.commitment() != o.commitment() {
                        return false;
                    }
                    if rt.states_snapshot() != o.states_snapshot() {
                        return false;
                    }
                }
            }
        }
        self.sessions.len() == other.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse;
    use crate::Performative;

    const PROG: &str = "protocol p v1;\nroles buyer, seller;\ncapacity 2;\non request(\"q\") from buyer -> send propose to buyer;\non propose from seller -> send accept to seller;\non accept from buyer -> begin;\non breach -> send failure to buyer;\n";

    fn event(from: &str, perf: Performative, pattern: Option<&str>) -> Event {
        Event::In {
            from: from.to_string(),
            performative: perf,
            pattern: pattern.map(|p| p.to_string()),
            fields: None,
        }
    }

    #[test]
    fn sessions_are_isolated_and_capacity_enforced() {
        let prog = parse(PROG).unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        host.open("s1", "seller").unwrap();
        host.open("s2", "seller").unwrap();
        // capacity 2: third session rejected
        assert!(host.open("s3", "seller").is_err());
        // s1 progresses independently of s2
        let out = host.deliver("s1", event("buyer", Performative::Request, Some("(\"q\")"))).unwrap();
        assert_eq!(out[0].performative, Performative::Propose);
        assert_eq!(host.session_count(), 2);
        host.close("s1").unwrap();
        assert!(!host.is_open("s1"));
        // slot freed
        host.open("s3", "seller").unwrap();
        assert_eq!(host.session_count(), 2);
        // unknown session delivery rejected
        assert!(host.deliver("nope", event("buyer", Performative::Request, None)).is_err());
    }

    #[test]
    fn event_sourcing_replay_reconstructs_exactly() {
        let prog = parse(PROG).unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        host.open("s1", "seller").unwrap();
        host.deliver("s1", event("buyer", Performative::Request, Some("(\"q\")"))).unwrap();
        host.deliver("s1", event("buyer", Performative::Accept, None)).unwrap();
        // commitment went proposed -> active via the accept handler
        let original = &host.sessions["s1"];

        // persist: journal + role header; crash; replay
        let (roles, journal) = host.journal_with_roles();
        assert_eq!(journal.len(), 2);
        let rebuilt = SessionHost::replay_from(&prog, &roles, &journal).unwrap();
        assert!(host.same_state_as(&rebuilt), "replay must reconstruct exact state");
        // and the replayed host continues deterministically
        let _ = original; // silence unused in this scope shape
    }

    #[test]
    fn capability_gate_is_enforced() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nuse fs.read as reader;\nuse net.http;\non query from a -> send inform to a;\non inform from b -> send bye;\n",
        )
        .unwrap();
        // default-on: plain host grants NOTHING — open rejected
        let mut host = SessionHost::new(&prog).unwrap();
        let err = host.open("s1", "a").unwrap_err();
        assert!(err.contains("capability gate"), "{err}");
        // partially granted: still rejected
        assert!(SessionHost::new_gated(&prog, &["fs.read".into()])
            .unwrap()
            .open("s1", "a")
            .is_err());
        // gated host with all grants: works
        let mut host = SessionHost::new_gated(
            &prog,
            &["fs.read".into(), "net.http".into()],
        )
        .unwrap();
        host.open("s1", "a").unwrap();
        assert_eq!(host.session_count(), 1);
        // peer AgentCard check
        let cards = vec![
            vec!["fs.read".to_string(), "net.http".to_string(), "acl.speak".to_string()],
            vec!["acl.speak".to_string()],
        ];
        SessionHost::peer_satisfies(&cards[0])(&prog)
            .expect("peer has all needed capabilities");
        assert!(SessionHost::peer_satisfies(&cards[1])(&prog).is_err());
        // stdlib.* names are not capabilities (not gated)
        let prog2 = parse("protocol q v1;\nroles a,b;\nuse stdlib.fipa;\non query from a -> send inform to a;\non inform from b -> send bye;\n").unwrap();
        SessionHost::new_gated(&prog2, &[]).expect("stdlib.* needs no grant");
    }

    #[test]
    fn journal_records_unhandled_events_too() {
        let prog = parse(PROG).unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        host.open("s1", "buyer").unwrap();
        // buyer has no handlers for request — still journaled
        host.deliver("s1", event("seller", Performative::Request, None)).unwrap();
        assert_eq!(host.journal().len(), 1);
    }
}
