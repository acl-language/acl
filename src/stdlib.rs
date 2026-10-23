//! §35.14: standard protocol fragments — battle-tested handler sets
//! for common agent interaction patterns. Merge into a program with
//! `apply_fragment`; the program's own handlers win on trigger clash.
//!
//! Fragments here are compiled Rust (not `.aclp`) because they are
//! polymorphic over the host program's roles.

use crate::protocol::{Guard, Handler, OnTrigger, ProtocolAction, ProtocolProgram};
use std::collections::BTreeMap;
use crate::Performative;

/// escalate to the counterpart when a commitment nears expiry:
/// `every <warn_ms> -> send progress to <other>`
pub fn deadline_escalation(warn_ms: i64) -> Handler {
    Handler {
        trigger: OnTrigger::Timer { every_ms: warn_ms, guard: None, as_role: None },
        guard: None,
        actions: vec![ProtocolAction::Send {
            performative: Performative::Progress,
            to: None,
            fields: Some(BTreeMap::from([(
                "reason".into(),
                "deadline approaching".into(),
            )])),
            retry: None,
        }],
        line: 0,
    }
}

/// require a second confirmation before `begin`: an `accept` only
/// activates the commitment when a `confirm` flag was set by a prior
/// `inform`.
pub fn double_confirmation() -> Handler {
    Handler {
        trigger: OnTrigger::Event {
            performative: Performative::Accept,
            pattern: None,
            from: None,
            as_role: None,
            phase: None,
        },
        guard: Some(Guard::Atom { name: "confirmed".into(), neg: false }),
        actions: vec![ProtocolAction::Begin],
        line: 0,
    }
}

/// register a fragment under a name
pub fn lookup(name: &str) -> Option<Vec<Handler>> {
    match name {
        "deadline_escalation" => Some(vec![deadline_escalation(60_000)]),
        "double_confirmation" => Some(vec![double_confirmation()]),
        _ => None,
    }
}

/// merge fragment handlers into a program, skipping exact duplicates
pub fn apply_fragment(program: &mut ProtocolProgram, name: &str) -> Result<usize, String> {
    let handlers = lookup(name).ok_or_else(|| format!("unknown fragment '{name}'"))?;
    let mut added = 0;
    for h in handlers {
        let exists = program.handlers.iter().any(|x| {
            x.trigger == h.trigger && x.actions == h.actions
        });
        if !exists {
            program.handlers.push(h);
            added += 1;
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse;

    #[test]
    fn fragments_merge_without_clash() {
        let mut prog = parse(
            "protocol p v1;\nroles a, b;\non query from a -> send inform to a;\non inform from b -> send bye;\n",
        )
        .unwrap();
        assert_eq!(apply_fragment(&mut prog, "deadline_escalation").unwrap(), 1);
        // second apply is a no-op (dedup)
        assert_eq!(apply_fragment(&mut prog, "deadline_escalation").unwrap(), 0);
        assert_eq!(apply_fragment(&mut prog, "double_confirmation").unwrap(), 1);
        assert_eq!(prog.handlers.len(), 4);
        assert!(lookup("nope").is_none());
    }
}
