//! §52: semantic parity — the Rust runtime and the generated Python agent
//! MUST produce identical observable behavior for the same input sequence,
//! INCLUDING nested actions (enter/begin/fulfil/interp send inside try),
//! exercised catch paths, and named-commitment lifecycles.
//! Requires a python3 interpreter; run with `cargo test --test py_parity -- --ignored`.

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
state caught: bool;
state items: list;
on inform {"n": $n} from a in start {
  try { set x = $n * 2; set y = x + 1; enter done; begin job; send inform {"r": "${y}"} to a; } catch { set caught = true; }
}
on inform {"n": $n} from a in done {
  try { push items, "p1"; push items, "p2"; for it in items { send inform {"i": "${it}"} to a; } set x = x / 0; } catch { set caught = true; }
  fulfil job;
}
every 1s as b while not flag -> set flag = true;
"#;

fn state_text(v: &StateValue) -> String {
    match v {
        StateValue::Bool(b) => b.to_string(),
        StateValue::Int(i) => i.to_string(),
        StateValue::Float(f) => f.to_string(),
        StateValue::Str(s) => s.clone(),
        StateValue::List(l) => {
            let items: Vec<String> = l
                .iter()
                .map(|v| match v {
                    StateValue::Str(s) => format!("\"{}\"", s),
                    StateValue::Int(i) => i.to_string(),
                    _ => "?".to_string(),
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        StateValue::Map(_) => "<map>".into(),
        StateValue::Record(_) => "<record>".into(),
        StateValue::Unset => "unset".into(),
    }
}

fn rust_run() -> (Vec<String>, BTreeMap<String, String>, BTreeMap<String, String>) {
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
        fields: Some(BTreeMap::from([("n".into(), FieldValue::Int(3))])),
    });
    for o in &o2 {
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
    let states: BTreeMap<String, String> = r
        .states_snapshot()
        .into_iter()
        .map(|(k, v)| (k, state_text(&v)))
        .collect();
    let job = r.commitment_status("job");
    let job_text = match job {
        Some(c) => format!("{c:?}").to_lowercase(),
        None => "absent".to_string(),
    };
    let commitments = BTreeMap::from([("job".to_string(), job_text)]);
    (outs, states, commitments)
}

#[test]
#[ignore = "requires python3; run with --ignored (CI runs it explicitly)"]
fn py_parity() {
    let prog = parse(PROGRAM).unwrap();
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
o2 = ag.deliver("a", "inform", None, {"n": 3})
for o in o2:
    f = o["fields"] or {}
    outs.append(str(o["to"]) + ":" + json.dumps({k: str(v) for k, v in sorted(f.items())}, separators=(",", ":")))
def st(v):
    if v is None: return "unset"
    if isinstance(v, bool): return "true" if v else "false"
    if isinstance(v, list):
        return "[" + ", ".join(json.dumps(x) if isinstance(x, str) else str(x) for x in v) + "]"
    return str(v)
states = {k: st(v) for k, v in ag.states.items()}
commitments = {k: str(v) for k, v in ag.commitments.items()}
print(json.dumps({"outs": outs, "states": states, "commitments": commitments}))
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

    let (rust_outs, rust_states, rust_commitments) = rust_run();
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
        let pv_norm = match rv.as_str() {
            "true" => pv.replace("True", "true"),
            "false" => pv.replace("False", "false"),
            _ => pv.to_string(),
        };
        assert_eq!(&pv_norm, rv, "STATE parity broken for {k}");
    }
    // parity: NAMED COMMITMENT lifecycles (§51)
    for (k, rv) in &rust_commitments {
        let pv = py["commitments"]
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("absent");
        assert_eq!(pv, rv, "COMMITMENT parity broken for {k}");
    }
    // the exercised paths MUST include: catch, phase transition, fulfil
    assert_eq!(
        rust_states.get("caught").map(String::as_str),
        Some("true"),
        "catch path must have run (flag is set by the timer, not the catch)"
    );
    assert!(
        rust_states.get("items").map(String::as_str).unwrap_or("").contains("p1"),
        "for body must have collected items"
    );
    assert_eq!(
        rust_commitments.get("job").map(String::as_str),
        Some("fulfilled"),
        "fulfil job must have run"
    );
}
