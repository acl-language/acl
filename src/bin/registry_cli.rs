//! acl-registry — CLI for the local agent registry.
//!   acl-registry <db> register <name> <aid> <pubkey_hex> [caps,comma]
//!   acl-registry <db> lookup <name>
//!   acl-registry <db> by-capability <cap>
//!   acl-registry <db> list

use acl_core::registry::{Registry, RegistryEntry};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: acl-registry <db> register|lookup|by-capability|list [args]");
        std::process::exit(2);
    }
    let r = Registry::open(&args[1]);
    match args[2].as_str() {
        "register" => {
            let caps: Vec<String> = args
                .get(6)
                .map(|c| c.split(',').map(String::from).collect())
                .unwrap_or_default();
            r.register(RegistryEntry {
                name: args[3].clone(),
                aid: args[4].clone(),
                public_key: args.get(5).cloned().unwrap_or_default(),
                capabilities: caps,
                registered_at_ms: 0,
            })
            .expect("register");
            println!("registered {}", args[3]);
        }
        "lookup" => match r.lookup(&args[3]) {
            Some(e) => println!("{}", serde_json::to_string_pretty(&e).unwrap()),
            None => std::process::exit(1),
        },
        "by-capability" => {
            for e in r.by_capability(&args[3]) {
                println!("{} {}", e.name, e.aid);
            }
        }
        "list" => {
            for e in r.list() {
                println!("{} {} caps={:?}", e.name, e.aid, e.capabilities);
            }
        }
        other => {
            eprintln!("unknown command {other}");
            std::process::exit(2);
        }
    }
}
