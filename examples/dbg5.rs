use acl_core::runtime::{Runtime, StateValue};
use acl_core::protocol::parse;
fn main() {
    let prog = parse("protocol p v1;\nroles a, b;\nstate w: Bid;\n").unwrap();
    let mut r = Runtime::new(&prog, "a").unwrap();
    let rec = StateValue::Record([("__type".to_string(), StateValue::Str("Bid".into())), ("price".to_string(), StateValue::Int(5))].into_iter().collect());
    let res = r.set_state("w", rec);
    println!("direct set_state: {:?}", res);
    println!("states: {:?}", r.states_snapshot());
}
