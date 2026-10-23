//! Formal semantics (spec §17): every performative carries machine-readable
//! feasibility preconditions (FP) and rational effects (RE), in the
//! FIPA-ACL tradition, so agents and verifiers can process the semantics
//! of a message rather than parse English prose.

use crate::Performative;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformativeSemantics {
    pub performative: Performative,
    /// What must be true of the SENDER's mental state for the message to
    /// be sincere/valid (feasibility precondition).
    pub feasibility_precondition: &'static str,
    /// The intended effect on the RECEIVER (rational effect).
    pub rational_effect: &'static str,
}

/// The semantic table for all 14 performatives.
pub fn semantics(p: Performative) -> PerformativeSemantics {
    use Performative::*;
    let (fp, re) = match p {
        Request => (
            "FP(request): sender intends that receiver perform act(a); sender believes receiver CAN perform act(a)",
            "RE(request): receiver intends to perform act(a)",
        ),
        Inform => (
            "FP(inform): sender believes proposition(p) is true; sender intends receiver to believe it",
            "RE(inform): receiver believes proposition(p)",
        ),
        Query => (
            "FP(query): sender does not know whether proposition(p) holds; sender wants to know",
            "RE(query): receiver informs sender of the truth of proposition(p)",
        ),
        Propose => (
            "FP(propose): sender intends receiver commit to act(a); act is beneficial to both under the stated terms",
            "RE(propose): receiver evaluates and accepts/rejects the commitment",
        ),
        Accept => (
            "FP(accept): a prior commitment exists with status=proposed and sender is its creditor",
            "RE(accept): commitment becomes active; debtor owes the act",
        ),
        Reject => (
            "FP(reject): a prior proposal exists and sender refuses it (reason optional)",
            "RE(reject): proposal is void; no commitment arises",
        ),
        Cancel => (
            "FP(cancel): a prior act is in flight and sender has authority to abort it",
            "RE(cancel): receiver stops the act and acknowledges",
        ),
        Progress => (
            "FP(progress): sender is executing an act and has intermediate state worth reporting",
            "RE(progress): receiver's estimate of completion is updated",
        ),
        InputRequired => (
            "FP(input_required): receiver cannot continue without X from the sender; receiver believes sender can supply X",
            "RE(input_required): sender supplies X (via inform) or cancels",
        ),
        Failure => (
            "FP(failure): receiver attempted act(a) and could not complete it; failure is terminal",
            "RE(failure): sender knows act(a) failed (and why); no result will come",
        ),
        Delegate => (
            "FP(delegate): sender has a chain of authority to sub-contract act(a) to receiver; chain recorded in body",
            "RE(delegate): receiver acts as sub-contractor; result flows back up the chain",
        ),
        Error => (
            "FP(error): a protocol/transport-level problem occurred that prevents semantic processing",
            "RE(error): sender corrects the protocol violation",
        ),
        Hello => (
            "FP(hello): sender is ready to communicate and its AgentCard (or hash) is attached",
            "RE(hello): receiver knows sender's identity and capabilities",
        ),
        Bye => (
            "FP(bye): sender has no pending obligations in this session",
            "RE(bye): session is closed; further messages require a new session",
        ),
    };
    PerformativeSemantics { performative: p, feasibility_precondition: fp, rational_effect: re }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum_like::all_performatives;

    #[test]
    fn every_performative_has_semantics() {
        for p in all_performatives() {
            let s = semantics(p);
            assert!(s.feasibility_precondition.starts_with("FP("));
            assert!(s.rational_effect.starts_with("RE("));
            assert_eq!(s.performative, p);
        }
    }

    #[test]
    fn specific_spot_checks() {
        let s = semantics(Performative::InputRequired);
        assert!(s.feasibility_precondition.contains("cannot continue"));
        let a = semantics(Performative::Accept);
        assert!(a.feasibility_precondition.contains("status=proposed"));
        let b = semantics(Performative::Bye);
        assert!(b.feasibility_precondition.contains("no pending obligations"));
    }
}

/// Tiny local iterator over all performatives (avoids a strum dependency).
pub mod strum_like {
    use crate::Performative;

    pub fn all_performatives() -> impl Iterator<Item = Performative> {
        use Performative::*;
        [
            Request, Inform, Query, Propose, Accept, Reject, Cancel, Progress,
            InputRequired, Failure, Delegate, Error, Hello, Bye,
        ]
        .into_iter()
    }
}
