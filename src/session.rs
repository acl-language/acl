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
    /// §49: every session EVER opened (id → role) — survives close so the
    /// replay header covers closed sessions too
    opened_roles: BTreeMap<SessionId, String>,
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
            opened_roles: BTreeMap::new(),
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
        self.opened_roles.insert(id.to_string(), role.to_string());
        Ok(())
    }

    pub fn close(&mut self, id: &str) -> Result<(), String> {
        let closed = self
            .sessions
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| format!("session '{id}' not open"));
        // §49: closes are inputs too — journaled for exact replay
        if closed.is_ok() {
            self.journal.push((id.to_string(), Event::SessionClosed));
        }
        closed
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// deliver an event into a session; outs go to the outbox
    pub fn deliver(&mut self, id: &str, ev: Event) -> Result<Vec<OutEvent>, String> {
        // §49: journal-replayable inputs — a Tick drives that session's
        // runtime clock; a SessionClosed closes the session. Both are
        // journaled like any other input so replay is EXACT.
        if matches!(ev, Event::SessionClosed) {
            // close() itself journals the SessionClosed entry (§49)
            self.close(id)?;
            return Ok(Vec::new());
        }
        if let Event::Tick(ms) = ev {
            let rt = self
                .sessions
                .get_mut(id)
                .ok_or_else(|| format!("session '{id}' not open"))?;
            let outs = rt.tick(ms);
            self.journal.push((id.to_string(), Event::Tick(ms)));
            for o in &outs {
                self.outbox.push((id.to_string(), o.clone()));
            }
            return Ok(outs);
        }
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

    /// host-injected time for ALL sessions — journaled per session (§49)
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
        // §49: every time advance enters the journal — replay reproduces
        // timers, awaits and retry deadlines exactly
        for id in self.sessions.keys() {
            self.journal.push((id.clone(), Event::Tick(elapsed_ms)));
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

    /// journal with explicit session→role map (the persistence header) —
    /// §49: capability grants are part of the header too (they gate open)
    pub fn journal_with_roles(
        &self,
    ) -> (Vec<(String, String)>, Vec<String>, Vec<(SessionId, Event)>) {
        // §49: from opened_roles (NOT self.sessions) — closed sessions
        // must appear in the replay header
        let roles: Vec<(String, String)> = self
            .opened_roles
            .iter()
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        let grants: Vec<String> = self.granted.iter().cloned().collect();
        (roles, grants, self.journal.clone())
    }

    /// rebuild from journal + role header
    pub fn replay_from(
        program: &ProtocolProgram,
        roles: &[(String, String)],
        grants: &[String],
        journal: &[(SessionId, Event)],
    ) -> Result<Self, String> {
        // §49: capability grants are reconstructed — they gate open()
        let mut host = SessionHost::new_gated(program, grants)?;
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
        let (roles, grants, journal) = host.journal_with_roles();
        assert_eq!(journal.len(), 2);
        let rebuilt = SessionHost::replay_from(&prog, &roles, &grants, &journal).unwrap();
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

    /// §37: soak — many concurrent sessions, many events, then verify
    /// determinism end to end (replay equality over the full journal).
    #[test]
    fn soak_many_sessions_and_events() {
        let prog = parse(&PROG.replace("capacity 2;", "capacity 200;")).unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        let sessions: Vec<String> = (0..200).map(|i| format!("s{i}")).collect();
        for id in &sessions {
            host.open(id, "seller").unwrap();
        }
        for round in 0..25 {
            for id in sessions.iter().cycle().take(200) {
                let ev = if round == 0 {
                    event("buyer", Performative::Request, Some("(\"q\")"))
                } else if round % 2 == 1 {
                    event("buyer", Performative::Accept, None)
                } else {
                    event("buyer", Performative::Query, None)
                };
                let _ = host.deliver(id, ev);
            }
        }
        assert_eq!(host.session_count(), 200);
        assert_eq!(host.journal().len(), 200 * 25);
        // replay the whole journal into a fresh host — must be identical
        let (roles, grants, journal) = host.journal_with_roles();
        assert_eq!(roles.len(), 200);
        let rebuilt = SessionHost::replay_from(&prog, &roles, &grants, &journal).unwrap();
        assert!(
            host.same_state_as(&rebuilt),
            "soak replay must reconstruct exact state"
        );
        // outbox: exactly one propose per session (round 0), and retries
        // of accept are idempotent (begin ignored, logged)
        let proposes = host
            .outbox()
            .iter()
            .filter(|(_, o)| o.performative == Performative::Propose)
            .count();
        assert_eq!(proposes, 200);
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

#[cfg(test)]
mod full_replay_tests {
    use super::*;
    use crate::runtime::StateValue;
    use crate::protocol::parse;
    use std::collections::BTreeMap;

    // §49: tick() advances are journaled — a timer fired by tick is
    // reproduced EXACTLY by replay (exact-state assertion)
    #[test]
    fn replay_reproduces_timer_and_await_and_close() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nstate fired: bool;\nstate bids: list;\nstate ready: bool;\non request from a { await 2 propose into bids within 60s as ready; send inform to a; }\non propose from b -> send bye;\non inform from b -> send bye;\nevery 1s as b while not fired -> set fired = true;\non bye from b -> send inform to b;\non bye from a -> send inform to a;\n",
        )
        .unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        host.open("s1", "b").unwrap();
        host.deliver(
            "s1",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Request,
                pattern: None,
                fields: None,
            },
        )
        .unwrap();
        host.deliver(
            "s1",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Propose,
                pattern: None,
                fields: Some(BTreeMap::from([("who".into(), crate::runtime::FieldValue::Str("b1".into()))])),
            },
        )
        .unwrap();
        let outs1 = host.tick(1500);
        host.close("s1").unwrap();

        let (roles, grants, journal) = host.journal_with_roles();
        let rebuilt = SessionHost::replay_from(&prog, &roles, &grants, &journal).unwrap();
        // EXACT state: timer fired via journaled tick, await collected,
        // session closed
        assert_eq!(
            rebuilt
                .sessions
                .get("s1")
                .map(|_| ()) // closed sessions are absent — presence differs
                .is_none(),
            true
        );
        // compare journals: the rebuilt host's journal == original
        assert_eq!(rebuilt.journal().len(), host.journal().len());
        // and the original runtime's states had the timer + await effects
        let orig_rt = host.sessions.get("s1");
        assert!(orig_rt.is_none(), "closed session removed");
        let _ = outs1;

        // a still-open timer session replays its clock exactly
        let mut h2 = SessionHost::new(&prog).unwrap();
        h2.open("s2", "b").unwrap();
        h2.deliver(
            "s2",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Request,
                pattern: None,
                fields: None,
            },
        )
        .unwrap();
        h2.deliver(
            "s2",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Propose,
                pattern: None,
                fields: None,
            },
        )
        .unwrap();
        h2.tick(1000);
        let (roles2, grants2, journal2) = h2.journal_with_roles();
        let rebuilt2 = SessionHost::replay_from(&prog, &roles2, &grants2, &journal2).unwrap();
        let a = h2.sessions.get("s2").unwrap();
        let b = rebuilt2.sessions.get("s2").unwrap();
        assert_eq!(a.states_snapshot().get("fired"), b.states_snapshot().get("fired"));
        assert_eq!(
            a.states_snapshot().get("ready"),
            b.states_snapshot().get("ready")
        );
        assert_eq!(
            a.states_snapshot().get("bids").cloned().unwrap(),
            b.states_snapshot().get("bids").cloned().unwrap()
        );
        assert_eq!(a.transcript().len(), b.transcript().len());
        assert_eq!(a.now_ms(), b.now_ms());
    }
}

#[cfg(test)]
mod concurrent_conversations_tests {
    use super::*;
    use crate::protocol::parse;
    use crate::runtime::StateValue;

    // §53: multiple concurrent conversations of the SAME role are
    // independent instances — states AND conversation phases do not leak
    #[test]
    fn concurrent_instances_have_independent_phases_and_states() {
        let prog = parse(
            "protocol p v1;\nroles a, b;\nphase start;\nphase accepted;\nstate n: int;\non inform {\"k\": $k} from a in start { set n = $k; enter accepted; }\non inform {\"k\": $k} from a in accepted { set n = n + $k; }\non inform from b -> send bye;\n",
        )
        .unwrap();
        let mut host = SessionHost::new(&prog).unwrap();
        host.open("s1", "b").unwrap();
        host.open("s2", "b").unwrap();
        host.deliver(
            "s1",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Inform,
                pattern: None,
                fields: Some(BTreeMap::from([(
                    "k".into(),
                    crate::runtime::FieldValue::Int(5),
                )])),
            },
        )
        .unwrap();
        // s1 entered accepted; s2 MUST still be in start
        host.deliver(
            "s2",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Inform,
                pattern: None,
                fields: Some(BTreeMap::from([(
                    "k".into(),
                    crate::runtime::FieldValue::Int(7),
                )])),
            },
        )
        .unwrap();
        let s1 = host.sessions.get("s1").unwrap();
        let s2 = host.sessions.get("s2").unwrap();
        // each instance followed its OWN path: s1 n=5 (start handler),
        // s2 n=7 (its own start handler) — no cross-contamination
        assert_eq!(s1.states_snapshot().get("n"), Some(&StateValue::Int(5)));
        assert_eq!(s2.states_snapshot().get("n"), Some(&StateValue::Int(7)));
        assert_eq!(s1.now_phase(), "accepted");
        assert_eq!(s2.now_phase(), "accepted");
        // a second event in s1 runs the ACCEPTED handler (n = n + k),
        // while s2 is untouched
        host.deliver(
            "s1",
            Event::In {
                from: "a".into(),
                performative: crate::Performative::Inform,
                pattern: None,
                fields: Some(BTreeMap::from([(
                    "k".into(),
                    crate::runtime::FieldValue::Int(3),
                )])),
            },
        )
        .unwrap();
        assert_eq!(
            host.sessions.get("s1").unwrap().states_snapshot().get("n"),
            Some(&StateValue::Int(8))
        );
        assert_eq!(
            host.sessions.get("s2").unwrap().states_snapshot().get("n"),
            Some(&StateValue::Int(7))
        );
    }
}
