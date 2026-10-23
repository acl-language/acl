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
        Request => &[Inform, InputRequired, Failure, Reject, Progress, Error],
        Delegate => &[Inform, InputRequired, Failure, Reject, Progress, Error],
        Query => &[Inform, Failure, Error],
        Propose => &[Accept, Reject, Error],
        Accept => &[Inform, Failure, Error],
        Cancel => &[Inform, Error],
        Inform => &[Inform, Error],
        Hello => &[Hello, Error],
        InputRequired => &[Inform, Cancel, Error],
        Progress => &[Progress, Inform, Failure, InputRequired, Error],
        Failure => &[Inform, Error],
        Reject => &[Inform, Error],
        Bye => &[],
        Error => &[Inform, Error],
    }
}

/// Is `reply` a legal move after `prev`?
pub fn dialogue_legal(prev: Performative, reply: Performative) -> bool {
    legal_replies(prev).contains(&reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Performative::*;

    #[test]
    fn core_matrix() {
        // request: answered by result/agreement-to-wait/refusal/progress
        assert!(dialogue_legal(Request, Inform));
        assert!(dialogue_legal(Request, InputRequired));
        assert!(dialogue_legal(Request, Failure));
        assert!(dialogue_legal(Request, Reject));
        assert!(dialogue_legal(Request, Progress));
        assert!(!dialogue_legal(Request, Hello));
        assert!(!dialogue_legal(Request, Bye));
        assert!(!dialogue_legal(Request, Accept));

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
