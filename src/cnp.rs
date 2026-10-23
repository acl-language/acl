//! Contract-Net Protocol (spec §21): the standard multi-agent negotiation
//! pattern (Smith 1980, FIPA Contract-Net) built on ACL performatives and
//! the content language.
//!
//! Phases:
//!   1. ANNOUNCE  manager → participants: request/Cl(action)  — task posted
//!   2. BID       participant → manager: propose/Cl(commitment{proposed})
//!   3. AWARD     manager → winner: accept/Cl(commitment{active})
//!                manager → losers: reject/Ref
//!   4. REPORT    winner → manager: inform/Cl(fact) or failure
//!
//! `CnpSession` tracks one announcement's lifecycle and validates phase
//! transitions (built on `dialogue_legal` + the commitment lifecycle).

use crate::cl::{ClAction, ClExpr, CommitmentStatus};
use crate::dialogue::dialogue_legal;
use crate::{Aid, Body, Envelope, Performative};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Announced,
    Bidding,
    Awarded,
    Reported,
    Failed,
}

/// Tracker for one contract-net session.
#[derive(Debug)]
pub struct CnpSession {
    pub task: String,
    pub manager: Aid,
    pub announcement_id: String,
    pub phase: Phase,
    pub bids: Vec<Aid>,
    pub winner: Option<Aid>,
}

impl CnpSession {
    pub fn new(task: &str, manager: Aid, announcement_id: String) -> Self {
        CnpSession {
            task: task.into(),
            manager,
            announcement_id,
            phase: Phase::Announced,
            bids: Vec::new(),
            winner: None,
        }
    }

    /// Manager posts the task: request/Cl(action).
    pub fn announce(
        &self,
        key: &ed25519_dalek::SigningKey,
        action: ClAction,
        participants: &[Aid],
    ) -> Vec<Envelope> {
        let mut out = Vec::new();
        for p in participants {
            let mut env = Envelope::new(
                self.manager.clone(),
                p.clone(),
                Performative::Request,
                Body::Cl {
                    expressions: vec![ClExpr::Action {
                        name: action.name.clone(),
                        params: action.params.clone(),
                        result_schema: action.result_schema.clone(),
                    }],
                },
            );
            env.sign(key).expect("sign");
            out.push(env);
        }
        out
    }

    /// Participant bids: propose/Cl(commitment{proposed}).
    /// Returns Err if the phase is wrong or the reply is off-policy.
    pub fn receive_bid(&mut self, env: &Envelope) -> Result<(), String> {
        if self.phase != Phase::Announced && self.phase != Phase::Bidding {
            return Err(format!("bid in phase {:?}", self.phase));
        }
        if !dialogue_legal(Performative::Request, env.performative) {
            return Err(format!("off-policy reply: {:?}", env.performative));
        }
        match (&env.performative, &env.body) {
            (Performative::Propose, Body::Cl { expressions }) => match &expressions[0] {
                ClExpr::Commitment { status: CommitmentStatus::Proposed, .. } => {
                    self.bids.push(env.from.clone());
                    self.phase = Phase::Bidding;
                    Ok(())
                }
                other => Err(format!("bid must be a proposed commitment, got {other:?}")),
            },
            (Performative::Reject, _) => Ok(()), // explicit refusal: fine, no bid
            (p, b) => Err(format!("bid must be propose/Cl(commitment), got {p:?}/{b:?}")),
        }
    }

    /// Manager awards the contract to one bidder: accept/Cl(commitment{active}).
    pub fn award(
        &mut self,
        key: &ed25519_dalek::SigningKey,
        winner: &Aid,
        act: &ClAction,
        deadline_ms: Option<i64>,
    ) -> Result<Envelope, String> {
        if self.phase != Phase::Bidding {
            return Err(format!("award in phase {:?}", self.phase));
        }
        if !self.bids.contains(winner) {
            return Err(format!("'{}' did not bid", winner.0));
        }
        self.winner = Some(winner.clone());
        self.phase = Phase::Awarded;
        let mut env = Envelope::new(
            self.manager.clone(),
            winner.clone(),
            Performative::Accept,
            Body::Cl {
                expressions: vec![ClExpr::commitment(
                    &winner.0,
                    &self.manager.0,
                    act.clone(),
                    deadline_ms,
                    CommitmentStatus::Active,
                )],
            },
        );
        env.in_reply_to = Some(self.announcement_id.clone());
        env.sign(key).expect("sign");
        Ok(env)
    }

    /// Winner reports completion: inform/Cl(fact).
    pub fn receive_report(&mut self, env: &Envelope) -> Result<(), String> {
        if self.phase != Phase::Awarded {
            return Err(format!("report in phase {:?}", self.phase));
        }
        if !dialogue_legal(Performative::Accept, env.performative) {
            return Err(format!("off-policy report: {:?}", env.performative));
        }
        match (&env.performative, &env.body) {
            (Performative::Inform, Body::Cl { expressions }) => {
                if matches!(expressions[0], ClExpr::Fact { truth: true, .. }) {
                    self.phase = Phase::Reported;
                    Ok(())
                } else {
                    self.phase = Phase::Failed;
                    Err("report fact has truth=false".into())
                }
            }
            (Performative::Failure, _) => {
                self.phase = Phase::Failed;
                Err("winner reported failure".into())
            }
            (p, b) => Err(format!("report must be inform/Cl(fact), got {p:?}/{b:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aid, Aid as _};
    use ed25519_dalek::SigningKey;
    use rand::Rng;
    use serde_json::json;

    fn key() -> SigningKey {
        let mut s = [0u8; 32];
        rand::rng().fill_bytes(&mut s);
        SigningKey::from_bytes(&s)
    }

    fn act() -> ClAction {
        ClAction {
            name: "acl.std.fs.file_read".into(),
            params: json!({"path": "/tmp/x"}),
            result_schema: None,
            partial: None,
        }
    }

    #[test]
    fn full_lifecycle() {
        let manager_key = key();
        let manager = Aid::local(1, "mgr");
        let worker = Aid::local(2, "wrk");
        let mut session = CnpSession::new("read-file", manager.clone(), "ann-1".into());

        // 1. announce
        let announcements = session.announce(&manager_key, act(), &[worker.clone()]);
        assert_eq!(announcements.len(), 1);
        assert!(announcements[0].verify(&manager_key.verifying_key()).unwrap());

        // 2. worker bids: propose/Cl(commitment{proposed})
        let mut bid = Envelope::new(
            worker.clone(),
            manager.clone(),
            Performative::Propose,
            Body::Cl {
                expressions: vec![ClExpr::commitment(
                    &worker.0,
                    &manager.0,
                    act(),
                    None,
                    CommitmentStatus::Proposed,
                )],
            },
        );
        bid.sign(&key()).unwrap(); // signed with worker key
        session.receive_bid(&bid).expect("bid");
        assert_eq!(session.bids.len(), 1);
        assert_eq!(session.phase, Phase::Bidding);

        // 3. award to the bidder
        let award = session.award(&manager_key, &worker, &act(), None).expect("award");
        assert!(dialogue_legal(Performative::Propose, Performative::Accept));
        assert!(award.verify(&manager_key.verifying_key()).unwrap());
        assert_eq!(session.phase, Phase::Awarded);

        // 4. report: inform/Cl(fact)
        let mut report = Envelope::new(
            worker.clone(),
            manager.clone(),
            Performative::Inform,
            Body::Cl {
                expressions: vec![ClExpr::fact("acl.std.task.task_status", json!({"msg_id": "ann-1"}))],
            },
        );
        report.sign(&key()).unwrap();
        session.receive_report(&report).expect("report");
        assert_eq!(session.phase, Phase::Reported);
    }

    #[test]
    fn award_without_bid_rejected() {
        let manager_key = key();
        let manager = Aid::local(1, "mgr");
        let stranger = Aid::local(9, "stranger");
        let mut session = CnpSession::new("t", manager, "ann".into());
        session.phase = Phase::Bidding;
        assert!(session.award(&manager_key, &stranger, &act(), None).is_err());
    }

    #[test]
    fn report_out_of_phase_rejected() {
        let mut session = CnpSession::new("t", Aid::local(1, "mgr"), "ann".into());
        let env = Envelope::new(
            Aid::local(2, "w"),
            Aid::local(1, "mgr"),
            Performative::Inform,
            Body::Cl {
                expressions: vec![ClExpr::fact("x", json!({}))],
            },
        );
        assert!(session.receive_report(&env).is_err());
    }

    #[test]
    fn off_policy_bid_rejected() {
        let mut session = CnpSession::new("t", Aid::local(1, "mgr"), "ann".into());
        let env = Envelope::new(
            Aid::local(2, "w"),
            Aid::local(1, "mgr"),
            Performative::Accept, // accept before any proposal = nonsense
            Body::Cl {
                expressions: vec![ClExpr::commitment(
                    "w", "mgr",
                    act(),
                    None,
                    CommitmentStatus::Active,
                )],
            },
        );
        assert!(session.receive_bid(&env).is_err());
    }
}
