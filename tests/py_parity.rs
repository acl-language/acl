//! §52: semantic parity — the Rust runtime and the generated Python agent
//! MUST produce identical observable behavior for the same input sequence.
//! Requires a python3 interpreter; ignored by default (`--ignored`).

use acl_core::protocol::parse;
use acl_core::runtime::{Event, FieldValue, Runtime, StateValue};
use std::collections::BTreeMap;

const PROGRAM: &str = r#"
protocol parity v1;
roles a, b;
phase start;
phase done;
state x: int;
state y: int;
state flag: bool;
on inform {"n": $n} from a in start {
  try { set x = $n * 2; set y = x + 1; } catch { set y = -1; }
  begin job;
  send inform {"r": "${y}"} to a;
}
on inform from a in done -> fulfil job;
every 1s as b while not flag -> set flag = true;
"#;

fn rust_run() -> (Vec<String>, BTreeMap<String, String>) {
    let prog = parse(PROGRAM).unwrap();
    let mut r = Runtime::new(&prog, "b").unwrap();
    let mut outs = Vec::new();
    let o1 = r.deliver(Event::In {
        from: "a".into(),
        performative: acl_core::Performative::Inform,
        pattern: None,
        fields: Some(BTreeMap::from([("n".into(), FieldValue::Int(5))])),
    });
    for o in &o1 {
        let f: BTreeMap<String, String> = o
            .fields
            .iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.text()))
            .collect();
        outs.push(format!(
            "{}:{}",
            o.to.as_deref().unwrap_or(""),
            serde_json::to_string(&f).unwrap_or_default()
        ));
    }
    r.tick(1500);
    let o2 = r.deliver(Event::In {
        from: "a".into(),
        performative: acl_core::Performative::Inform,
        pattern: None,
        fields: None,
    });
    for o in &o2 {
        outs.push(format!("{}", acl_core::runtime::perf_name_pub(o.performative)));
    }
    let states: BTreeMap<String, String> = r
        .states_snapshot()
        .into_iter()
        .map(|(k, v)| (k, state_text(&v)))
        .collect();
    (outs, states)
}

fn state_text(v: &StateValue) -> String {
    match v {
        StateValue::Bool(b) => b.to_string(),
        StateValue::Int(i) => i.to_string(),
        StateValue::Float(f) => f.to_string(),
        StateValue::Str(s) => s.clone(),
        StateValue::List(_) => "<list>".into(),
        StateValue::Map(_) => "<map>".into(),
        StateValue::Record(_) => "<record>".into(),
        StateValue::Unset => "unset".into(),
    }
}

#[test]
#[ignore = "requires python3"]
fn py_parity() {
    let prog = parse(PROGRAM).unwrap();
    // generate the agent for role b
    let code = acl_core::codegen::gen_agent_py(&prog, "b").expect("codegen");
    let dir = std::env::temp_dir().join("acl_py_parity");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let agent_path = dir.join("agent_b.py");
    std::fs::write(&agent_path, code).unwrap();
    let driver = dir.join("driver.py");
    std::fs::write(
        &driver,
        r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("agent_b", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
ag = m.Agent()
outs = []
o1 = ag.deliver("a", "inform", None, {"n": 5})
for o in o1:
    f = o["fields"] or {}
    outs.append(str(o["to"]) + ":" + json.dumps({k: str(v) for k, v in sorted(f.items())}, separators=(",", ":")))
ag.tick(1500)
o2 = ag.deliver("a", "inform", None, None)
for o in o2:
    outs.append(o["performative"])
states = {k: ("unset" if v is None else str(v).lower() if isinstance(v, bool) else str(v)) for k, v in ag.states.items()}
print(json.dumps({"outs": outs, "states": states, "commitments": ag.commitments}))
"#,
    )
    .unwrap();
    let out = std::process::Command::new("python3")
        .arg(&driver)
        .arg(&agent_path)
        .output()
        .expect("python3");
    assert!(out.status.success(), "py driver failed: {}", String::from_utf8_lossy(&out.stderr));
    let py: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let (rust_outs, rust_states) = rust_run();
    // parity: outgoing events
    let py_outs: Vec<String> = py["outs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(py_outs, rust_outs, "OUT-SEQUENCE parity broken");
    // parity: states
    for (k, rv) in &rust_states {
        let pv = py["states"].get(k).and_then(|v| v.as_str()).unwrap_or("missing");
        // py booleans print as True/False via str(); normalize
        let pv_norm = match rv.as_str() {
            "true" => pv.replace("True", "true"),
            "false" => pv.replace("False", "false"),
            _ => pv.to_string(),
        };
        assert_eq!(&pv_norm, rv, "STATE parity broken for {k}");
    }
}
