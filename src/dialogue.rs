//! Dialogue rules (spec §16): normative conversation policy.
//!
//! A language has discourse structure: after a given performative, only
//! certain replies are legal. `dialogue_legal(prev, reply)` is the machine-
//! checkable form; receivers SHOULD reject off-policy replies, sessions
//! MAY enforce it strictly.

use crate::Performative;

/// Replies legal immediately after `prev` (the client's turn) — the
/// server's legal next moves.
pub fn legal_replies(prev: Performative) -> &'static [Performative] {
    use Performative::*;
    match prev {
        Request => &[Inform, InputRequired, Failure, Reject, Propose, Progress, Error, Bye],
        Delegate => &[Inform, InputRequired, Failure, Reject, Propose, Progress, Error, Bye],
        Query => &[Inform, Failure, Error, Bye],
        Propose => &[Accept, Reject, Error, Bye],
        Accept => &[Inform, Failure, Error, Bye],
        Cancel => &[Inform, Error, Bye],
        Inform => &[Inform, Error, Retract, Bye],
        Hello => &[Hello, Error, Bye],
        InputRequired => &[Inform, Cancel, Error, Bye],
        Progress => &[Progress, Inform, Failure, InputRequired, Error, Bye],
        Failure => &[Inform, Error, Bye],
        Reject => &[Inform, Error, Bye],
        Bye => &[],
        Error => &[Inform, Error, Bye],
        Retract => &[Inform, Error, Bye],
        Subscribe => &[Inform, Failure, Error, Bye],
        Unsubscribe => &[Inform, Error, Bye],
    }
}

/// Is `reply` a legal move after `prev`?
pub fn dialogue_legal(prev: Performative, reply: Performative) -> bool {
    legal_replies(prev).contains(&reply)
}

/// §16 enforcement: tracks the last performative this side SENT and
/// validates each incoming reply against the matrix. Off-policy replies
/// are reported (receivers SHOULD reject them; strict enforcement is a
/// session policy choice).
#[derive(Debug, Default, Clone)]
pub struct DialogueTracker {
    /// last performative this side sent (the one awaiting a reply)
    pub last_out: Option<Performative>,
    /// session openers always reset expectations (§16: new conversation)
    openers: bool,
}

impl DialogueTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// record an outgoing performative (it now awaits a legal reply)
    pub fn on_send(&mut self, p: Performative) {
        self.last_out = Some(p);
    }

    /// validate an incoming reply against what we last sent.
    /// `Ok(true)` = legal reply, `Ok(false)` = no expectation open
    /// (peer opened a new conversation — legal), `Err` = off-policy.
    pub fn on_receive(&self, reply: Performative) -> Result<bool, (Performative, Performative)> {
        match self.last_out {
            None => Ok(true), // peer opened the conversation
            Some(prev) => {
                if dialogue_legal(prev, reply) {
                    Ok(true)
                } else {
                    Err((prev, reply))
                }
            }
        }
    }

    /// clear the expectation (e.g. after receiving the final answer)
    pub fn close(&mut self) {
        self.last_out = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Performative::*;

    #[test]
    fn tracker_rejects_off_policy_replies() {
        let mut t = DialogueTracker::new();
        // we sent a Propose: peer may only Accept/Reject/Error/Bye
        t.on_send(Performative::Propose);
        assert!(t.on_receive(Performative::Accept).is_ok());
        assert!(t.on_receive(Performative::Inform).is_err());
        assert!(t.on_receive(Performative::Request).is_err());
        // no expectation: anything is legal (peer opened)
        let t2 = DialogueTracker::new();
        assert!(t2.on_receive(Performative::Request).is_ok());
        // close clears
        t.close();
        assert!(t.on_receive(Performative::Request).is_ok());
    }

    #[test]
    fn core_matrix() {
        // request: answered by result/agreement-to-wait/refusal/progress
        assert!(dialogue_legal(Request, Inform));
        assert!(dialogue_legal(Request, InputRequired));
        assert!(dialogue_legal(Request, Failure));
        assert!(dialogue_legal(Request, Reject));
        assert!(dialogue_legal(Request, Progress));
        assert!(!dialogue_legal(Request, Hello));
        assert!(!dialogue_legal(Request, Accept));
        // §34: any participant may close the session — Bye is legal
        // after anything except after Bye itself
        assert!(dialogue_legal(Request, Bye));

        // query must be answered with information or failure — never Accept
        assert!(dialogue_legal(Query, Inform));
        assert!(!dialogue_legal(Query, InputRequired));
        assert!(!dialogue_legal(Query, Progress));

        // propose: only accept/reject — a result would be off-policy
        assert!(dialogue_legal(Propose, Accept));
        assert!(dialogue_legal(Propose, Reject));
        assert!(!dialogue_legal(Propose, Inform));

        // input_required: the client answers inform or cancels
        assert!(dialogue_legal(InputRequired, Inform));
        assert!(dialogue_legal(InputRequired, Cancel));
        assert!(!dialogue_legal(InputRequired, Failure));

        // bye closes: nothing may follow
        assert!(legal_replies(Bye).is_empty());
        assert!(!dialogue_legal(Bye, Inform));
        // session termination is always available (except after bye)
        for p in [Request, Inform, Propose, Accept, Failure, Error, Query] {
            assert!(dialogue_legal(p, Bye), "Bye must be legal after {p:?}");
        }

        // error: the peer may recover with inform
        assert!(dialogue_legal(Error, Inform));
        assert!(!dialogue_legal(Error, Hello));
    }

    #[test]
    fn progress_chain() {
        // a working agent keeps reporting progress until terminal
        assert!(dialogue_legal(Progress, Progress));
        assert!(dialogue_legal(Progress, Inform));
        assert!(dialogue_legal(Progress, Failure));
        assert!(dialogue_legal(Progress, InputRequired));
    }
}
