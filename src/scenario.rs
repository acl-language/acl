//! §35.12: scenario runner — executable protocol tests inside `.aclp`.
//!
//! A scenario drives projected runtimes exactly like the wire would:
//! `buyer sends request to seller` delivers into seller's runtime; the
//! outs queue up; `expect seller sends propose` pops the queue. Because
//! the runtime is deterministic, scenarios are real tests, not mocks.

use crate::protocol::{ProtocolProgram, Scenario, ScenarioStep};
use crate::runtime::{Event, OutEvent, Runtime};
use crate::Performative;
use std::collections::BTreeMap;

/// run one scenario; returns the trace on success, errors on failure.
pub fn run(prog: &ProtocolProgram, sc: &Scenario) -> Result<Vec<String>, Vec<String>> {
    let mut errors = Vec::new();
    let mut trace = Vec::new();
    // one runtime per role (lazily opened)
    let mut runtimes: BTreeMap<String, Runtime> = BTreeMap::new();
    // pending outs: (sender_role, OutEvent)
    let mut outbox: Vec<(String, OutEvent)> = Vec::new();

    for (i, step) in sc.steps.iter().enumerate() {
        match step {
            ScenarioStep::Send { from, performative, pattern, to } => {
                if !prog.roles.contains(from) {
                    errors.push(format!("step {}: undeclared role '{from}'", i + 1));
                    continue;
                }
                if !prog.roles.contains(to) {
                    errors.push(format!("step {}: undeclared role '{to}'", i + 1));
                    continue;
                }
                let rt = runtimes.entry(to.clone()).or_insert_with(|| {
                    Runtime::new(prog, to).expect("role checked above")
                });
                let outs = rt.deliver(Event::In {
                    from: from.clone(),
                    performative: *performative,
                    pattern: pattern.clone(),
                    fields: None,
                });
                trace.push(format!("step {}: {from} sends {} to {to} -> {} out", i + 1, crate::runtime::perf_name_pub(*performative), outs.len()));
                for o in outs {
                    outbox.push((to.clone(), o));
                }
            }
            ScenarioStep::ExpectSend { from, performative } => {
                let idx = outbox
                    .iter()
                    .position(|(sender, o)| sender == from && o.performative == *performative);
                match idx {
                    Some(idx) => {
                        let (sender, o) = outbox.remove(idx);
                        // the out event DELIVERS to its target (the wire!)
                        if let Some(t) = &o.to {
                            let rt = runtimes.entry(t.clone()).or_insert_with(|| {
                                Runtime::new(prog, t).expect("role checked")
                            });
                            let back = rt.deliver(Event::In {
                                from: sender.clone(),
                                performative: o.performative,
                                pattern: None,
                                fields: o.fields.clone(),
                            });
                            for b in back {
                                outbox.push((t.clone(), b));
                            }
                        }
                        trace.push(format!("step {}: expected {from} sends {} ✓", i + 1, crate::runtime::perf_name_pub(*performative)));
                    }
                    None => errors.push(format!(
                        "step {}: expected {} to send {} — outbox has {:?}",
                        i + 1,
                        from,
                        crate::runtime::perf_name_pub(*performative),
                        outbox.iter().map(|(s, o)| (s.as_str(), crate::runtime::perf_name_pub(o.performative))).collect::<Vec<_>>()
                    )),
                }
            }
            ScenarioStep::ExpectState { role, name, value } => {
                let Some(rt) = runtimes.get(role) else {
                    errors.push(format!("step {}: role '{role}' has no runtime yet", i + 1));
                    continue;
                };
                let actual = match rt.state(name) {
                    Some(crate::runtime::StateValue::Bool(b)) => *b,
                    Some(crate::runtime::StateValue::Unset) | None => false,
                    Some(other) => {
                        errors.push(format!("step {}: state '{name}' is {other:?}, not bool", i + 1));
                        continue;
                    }
                };
                if actual != *value {
                    errors.push(format!(
                        "step {}: expect state {role}.{name} == {value}, got {actual}",
                        i + 1
                    ));
                } else {
                    trace.push(format!("step {}: state {role}.{name} == {value} ✓", i + 1));
                }
            }
            ScenarioStep::ExpectLog { contains } => {
                // §47: the substring must appear in SOME role's transcript
                let found = runtimes.values().any(|rt| {
                    rt.transcript().iter().any(|e| e.text.contains(contains.as_str()))
                });
                if found {
                    trace.push(format!("step {}: log contains {:?} ✓", i + 1, contains));
                } else {
                    errors.push(format!(
                        "step {}: expect log {contains:?} — not found in any transcript",
                        i + 1
                    ));
                }
            }
            ScenarioStep::ExpectCommitment { role, status } => {
                let Some(rt) = runtimes.get(role) else {
                    errors.push(format!("step {}: role '{role}' has no runtime yet", i + 1));
                    continue;
                };
                let actual = format!("{:?}", rt.commitment()).to_lowercase();
                if actual != *status {
                    errors.push(format!(
                        "step {}: expect commitment {role} = {status}, got {actual}",
                        i + 1
                    ));
                } else {
                    trace.push(format!("step {}: commitment {role} = {status} ✓", i + 1));
                }
            }
        }
    }
    if errors.is_empty() { Ok(trace) } else { Err(errors) }
}

/// run ALL scenarios in a program; returns (passed, failed) names.
pub fn run_all(prog: &ProtocolProgram) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for sc in &prog.scenarios {
        match run(prog, sc) {
            Ok(_) => passed.push(sc.name.clone()),
            Err(errors) => failed.push((sc.name.clone(), errors)),
        }
    }
    (passed, failed)
}

/// silence unused import when Performative is only used in traces
#[allow(dead_code)]
fn _touch(_: Performative) {}

#[cfg(test)]
mod tests {
    use crate::protocol::parse;

    const SCENARIO_PROG: &str = r#"
protocol qa v1;
roles buyer, seller;
on request("q") from buyer -> send propose to buyer;
on propose from seller -> send accept to seller;
on accept from buyer -> begin;

scenario "happy path" {
  buyer sends request("q") to seller;
  expect seller sends propose;
  expect buyer sends accept;
  expect commitment seller active;
}

scenario "broken" {
  buyer sends request("q") to seller;
  expect seller sends accept;
}
"#;

    #[test]
    fn happy_scenario_passes() {
        let prog = parse(SCENARIO_PROG).unwrap();
        assert_eq!(prog.scenarios.len(), 2);
        let (passed, failed) = crate::scenario::run_all(&prog);
        assert_eq!(passed, vec!["happy path".to_string()]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "broken");
        // the failure names the exact step and the actual outbox
        assert!(failed[0].1.iter().any(|e| e.contains("expected seller to send accept")));
    }
}
