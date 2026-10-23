//! aclc — the ACL language compiler (spec §16 toolchain).
//!
//!   aclc check <file.acl>...          parse + typecheck (exit 0/1)
//!   aclc lint  <file.acl>...          check + style rules (doc on every decl)
//!   aclc fmt   <file.acl>...          canonical pretty-print in place
//!   aclc gen   <rust|py|ts> <file.acl> <out_dir>
//!   aclc names <file.acl>...          print fully-qualified names

use acl_core::idl::{self, IdlFile};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: aclc check|lint|fmt|gen|names|protocol <args>");
        std::process::exit(2);
    }
    let cmd = args[1].as_str();
    let files = &args[2..];

    match cmd {
        "endpoints" => {
            // §33.5: project the global protocol onto each role's endpoint
            let mut failed = false;
            for f in files {
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {f}: {e}");
                    std::process::exit(2);
                });
                match acl_core::protocol::parse(&src) {
                    Err(errors) => {
                        failed = true;
                        for e in errors { eprintln!("{f}:{e}"); }
                    }
                    Ok(prog) => {
                        for role in &prog.roles {
                            match acl_core::runtime::project(&prog, role) {
                                Err(errors) => {
                                    failed = true;
                                    for e in errors { eprintln!("{f}:{e}"); }
                                }
                                Ok(ep) => {
                                    println!("{f}: endpoint '{}' — {} handlers", ep.role, ep.handlers.len());
                                    for h in &ep.handlers {
                                        let t = match &h.trigger {
                                            acl_core::protocol::OnTrigger::Event { performative, pattern, from } => {
                                                format!("{:?}{} from {:?}", performative, pattern.clone().unwrap_or_default(), from)
                                            }
                                            acl_core::protocol::OnTrigger::Breach => "Breach".into(),
                                            acl_core::protocol::OnTrigger::Expired => "Expired".into(),
                                            acl_core::protocol::OnTrigger::Timer { every_ms, guard, .. } => {
                                                format!("Timer({}ms, guard:{guard:?})", every_ms)
                                            }
                                        };
                                        let a: Vec<String> = h.actions.iter().map(|x| match x {
                                            acl_core::protocol::ProtocolAction::Send { performative, to, .. } =>
                                                format!("send {:?} to {:?}", performative, to),
                                            acl_core::protocol::ProtocolAction::Begin => "begin".into(),
                                            acl_core::protocol::ProtocolAction::Fulfil => "fulfil".into(),
                                            acl_core::protocol::ProtocolAction::Retract { target } => format!("retract({target})"),
                                            acl_core::protocol::ProtocolAction::Return { .. } => "return".into(),
                                            acl_core::protocol::ProtocolAction::Set { name, .. } => format!("set {name}"),
                                            acl_core::protocol::ProtocolAction::Push { state, .. } => format!("push {state}"),
                                            acl_core::protocol::ProtocolAction::Put { state, key, .. } => format!("put {state}[{key}]"),
                                            acl_core::protocol::ProtocolAction::For { var, over, .. } => format!("for {var} in {over}"),
                                            acl_core::protocol::ProtocolAction::Abort => "abort".into(),
                                        }).collect();
                                        println!("    on {} -> {}", t, a.join("; "));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            std::process::exit(if failed { 1 } else { 0 });
        }
        "publish" | "protocols" | "fetch" => {
            // §35.15: protocol registry — publish/list/fetch (checksummed)
            let cmd = cmd;
            match cmd {
                "publish" => {
                    if files.len() < 3 {
                        eprintln!("usage: aclc publish <file.aclp> <registry.json> [publisher]");
                        std::process::exit(2);
                    }
                    let src = std::fs::read_to_string(&files[0]).unwrap_or_else(|e| {
                        eprintln!("aclc: cannot read {}: {e}", files[0]);
                        std::process::exit(2);
                    });
                    let mut reg = acl_core::registry_protocols::ProtocolRegistry::load(Path::new(&files[1]))
                        .unwrap_or_else(|e| { eprintln!("registry: {e}"); std::process::exit(2); });
                    match reg.publish(&src, files.get(2).map(|s| s.as_str())) {
                        Ok((name, ver)) => {
                            reg.save(Path::new(&files[1])).unwrap_or_else(|e| { eprintln!("save: {e}"); std::process::exit(2); });
                            println!("published {}@v{}", name, ver);
                        }
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    }
                }
                "protocols" => {
                    let reg = acl_core::registry_protocols::ProtocolRegistry::load(Path::new(files.get(0).map(|s| s.as_str()).unwrap_or("protocols.json")))
                        .unwrap_or_else(|e| { eprintln!("registry: {e}"); std::process::exit(2); });
                    for (name, ver, roles) in reg.list() {
                        println!("{}@v{} roles=[{}]", name, ver, roles.join(","));
                    }
                    if reg.is_empty() { println!("(empty registry)"); }
                }
                "fetch" => {
                    if files.len() < 3 {
                        eprintln!("usage: aclc fetch <name> <version> <registry.json>");
                        std::process::exit(2);
                    }
                    let reg = acl_core::registry_protocols::ProtocolRegistry::load(Path::new(&files[2]))
                        .unwrap_or_else(|e| { eprintln!("registry: {e}"); std::process::exit(2); });
                    let ver: u32 = files[1].trim_start_matches('v').parse().unwrap_or_else(|_| {
                        eprintln!("bad version '{}'", files[1]);
                        std::process::exit(2);
                    });
                    match reg.fetch_verified(&files[0], ver) {
                        Ok(prog) => println!("fetched {}@v{} (integrity verified): roles=[{}]", prog.name, prog.version, prog.roles.join(",")),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    }
                }
                _ => unreachable!(),
            }
        }
        "scenarios" => {
            // §35.12: run every scenario in the program (deterministic)
            let mut failed = false;
            for f in files {
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {f}: {e}");
                    std::process::exit(2);
                });
                match acl_core::protocol::parse(&src) {
                    Err(errors) => {
                        failed = true;
                        for e in errors { eprintln!("{f}:{e}"); }
                    }
                    Ok(prog) => {
                        let (passed, failures) = acl_core::scenario::run_all(&prog);
                        for name in &passed {
                            println!("{f}: scenario '{}' PASS", name);
                        }
                        for (name, errors) in &failures {
                            failed = true;
                            println!("{f}: scenario '{}' FAIL", name);
                            for e in errors {
                                eprintln!("    {e}");
                            }
                        }
                        if prog.scenarios.is_empty() {
                            println!("{f}: (no scenarios declared)");
                        }
                    }
                }
            }
            std::process::exit(if failed { 1 } else { 0 });
        }
        "trace" => {
            // §35.15: protocol debugger — replay a journal step by step.
            // journal file: one JSON event per line:
            //   {"role":"seller","from":"buyer","performative":"request"}
            let mut failed = false;
            for pair in files.chunks(2) {
                if pair.len() < 2 {
                    eprintln!("usage: aclc trace <file.aclp> <journal.jsonl>");
                    std::process::exit(2);
                }
                let (f, jf) = (&pair[0], &pair[1]);
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {f}: {e}");
                    std::process::exit(2);
                });
                let journal_src = std::fs::read_to_string(jf).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {jf}: {e}");
                    std::process::exit(2);
                });
                let prog = match acl_core::protocol::parse(&src) {
                    Ok(p) => p,
                    Err(errors) => {
                        failed = true;
                        for e in errors { eprintln!("{f}:{e}"); }
                        continue;
                    }
                };
                // parse journal lines
                let mut events: Vec<(String, acl_core::runtime::Event)> = Vec::new();
                let mut roles: Vec<(String, String)> = Vec::new();
                for (n, line) in journal_src.lines().enumerate() {
                    if line.trim().is_empty() { continue; }
                    let v: serde_json::Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(e) => { eprintln!("{jf}:{n}: {e}"); failed = true; continue; }
                    };
                    if let Some(mode) = v.get("open") {
                        roles.push((v["session"].as_str().unwrap_or("s").into(), mode.as_str().unwrap_or("buyer").into()));
                        continue;
                    }
                    let perf = v["performative"].as_str().unwrap_or("inform");
                    let perf = match perf {
                        "request" => acl_core::Performative::Request,
                        "inform" => acl_core::Performative::Inform,
                        "query" => acl_core::Performative::Query,
                        "propose" => acl_core::Performative::Propose,
                        "accept" => acl_core::Performative::Accept,
                        "reject" => acl_core::Performative::Reject,
                        "progress" => acl_core::Performative::Progress,
                        "failure" => acl_core::Performative::Failure,
                        "error" => acl_core::Performative::Error,
                        "bye" => acl_core::Performative::Bye,
                        _ => acl_core::Performative::Inform,
                    };
                    events.push((
                        v["session"].as_str().unwrap_or("s").to_string(),
                        acl_core::runtime::Event::In {
                            from: v["from"].as_str().unwrap_or("peer").to_string(),
                            performative: perf,
                            pattern: v["pattern"].as_str().map(|s| s.to_string()),
                            fields: v["fields"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()),
                        },
                    ));
                }
                match acl_core::session::SessionHost::replay_from(&prog, &roles, &events) {
                    Ok(host) => {
                        println!("{f}: replayed {} events across {} sessions", events.len(), host.session_count());
                        for (id, rt) in host.sessions_iter() {
                            println!("  session {id} (role {}): commitment={:?}, {} transcript entries", rt.role, rt.commitment(), rt.transcript().len());
                            for e in rt.transcript() {
                                println!("    {:>3} {:?} {}", e.seq, e.dir, e.text);
                            }
                        }
                    }
                    Err(e) => {
                        failed = true;
                        eprintln!("{f}: replay failed: {e}");
                    }
                }
            }
            std::process::exit(if failed { 1 } else { 0 });
        }
        "protocol" => {
            // §33: compile .aclp protocol programs — parse + §16 dialogue
            // matrix enforced at COMPILE time
            let mut failed = false;
            for f in files {
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {f}: {e}");
                    std::process::exit(2);
                });
                match acl_core::protocol::parse(&src) {
                    Err(errors) => {
                        failed = true;
                        for e in errors {
                            eprintln!("{f}:{e}");
                        }
                    }
                    Ok(prog) => match acl_core::protocol::check(&prog) {
                        Err(errors) => {
                            failed = true;
                            for e in errors {
                                eprintln!("{f}:{e}");
                            }
                        }
                        Ok(warnings) => {
                            for w in warnings {
                                eprintln!("{f}: warning: {w}");
                            }
                            println!("{f}: protocol '{}' v{} OK ({} roles, {} handlers)",
                                prog.name, prog.version, prog.roles.len(), prog.handlers.len());
                        }
                    },
                }
            }
            std::process::exit(if failed { 1 } else { 0 });
        }
        "check" | "lint" | "names" | "fmt" => {
            let mut failed = false;
            for f in files {
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {f}: {e}");
                    std::process::exit(2);
                });
                match idl::parse(&src) {
                    Err(errors) => {
                        failed = true;
                        for e in errors {
                            eprintln!("{f}:{e}");
                        }
                    }
                    Ok(file) => match idl::typecheck(&file) {
                        Err(errors) => {
                            failed = true;
                            for e in errors {
                                eprintln!("{f}:{e}");
                            }
                        }
                        Ok(names) => {
                            if cmd == "lint" {
                                for d in &file.decls {
                                    let doc_missing = match d {
                                        idl::Decl::Predicate { doc, .. }
                                        | idl::Decl::Action { doc, .. }
                                        | idl::Decl::Workflow { doc, .. } => doc.is_none(),
                                        idl::Decl::Alias { .. } => false,
                                    };
                                    if doc_missing {
                                        let local = match d {
                                            idl::Decl::Predicate { local, .. }
                                            | idl::Decl::Action { local, .. } => local.clone(),
                                        | idl::Decl::Workflow { local, .. } => local.clone(),
                                            idl::Decl::Alias { local, .. } => local.clone(),
                                        };
                                        eprintln!("{f}: lint: decl '{local}' missing doc comment");
                                        failed = true;
                                    }
                                }
                            }
                            if cmd == "fmt" {
                                let canonical = idl::pretty(&file);
                                if canonical != src {
                                    std::fs::write(f, &canonical).expect("write");
                                    eprintln!("{f}: reformatted");
                                }
                            }
                            if cmd == "names" {
                                for n in names {
                                    println!("{n}");
                                }
                            }
                            if cmd == "check" {
                                eprintln!("{f}: ok ({} decls)", file.decls.len());
                            }
                        }
                    },
                }
            }
            std::process::exit(if failed { 1 } else { 0 });
        }
        "gen" => {
            if files.len() != 3 {
                eprintln!("usage: aclc gen <rust|py|ts|schema> <file.acl> <out_dir>");
                std::process::exit(2);
            }
            let lang = files[0].as_str();
            if lang == "agent" {
                // §34: gen agent py <file.aclp> <out_dir> — one runnable
                // Python skeleton per role (BEFORE the IDL parse: .aclp
                // files are protocol programs, not vocabularies)
                let src = std::fs::read_to_string(&files[1]).unwrap_or_else(|e| {
                    eprintln!("aclc: cannot read {}: {e}", files[1]);
                    std::process::exit(2);
                });
                let out_dir = &files[2];
                match acl_core::protocol::parse(&src) {
                    Err(errors) => {
                        for e in errors { eprintln!("{}:{e}", files[0]); }
                        std::process::exit(1);
                    }
                    Ok(prog) => {
                        match acl_core::protocol::check(&prog) {
                            Err(errors) => {
                                for e in errors { eprintln!("{}:{e}", files[1]); }
                                std::process::exit(1);
                            }
                            Ok(_) => {}
                        }
                        std::fs::create_dir_all(out_dir).expect("mkdir");
                        let mut failed = false;
                        for role in &prog.roles {
                            match acl_core::codegen::gen_agent_py(&prog, role) {
                                Err(errors) => {
                                    failed = true;
                                    for e in errors { eprintln!("{}:{e}", files[1]); }
                                }
                                Ok(code) => {
                                    let path = format!("{out_dir}/agent_{}.py", role.replace('-', "_"));
                                    if let Err(e) = std::fs::write(&path, &code) {
                                        eprintln!("aclc: cannot write {path}: {e}");
                                        failed = true;
                                    } else {
                                        println!("{path}");
                                    }
                                }
                            }
                        }
                        std::process::exit(if failed { 1 } else { 0 });
                    }
                }
            }
            if files.len() != 3 {
                eprintln!("usage: aclc gen <rust|py|ts|schema|agent> <file> <out_dir>");
                std::process::exit(2);
            }
            let src = std::fs::read_to_string(&files[1]).expect("read");
            let file: IdlFile = idl::parse(&src).expect("parse");
            idl::typecheck(&file).expect("typecheck");
            let out_dir = &files[2];
            std::fs::create_dir_all(out_dir).expect("mkdir");
            let ns_file = file.namespace.replace('.', "_");
            let (content, fname) = match lang {
                "rust" => (idl::gen_rust(&file), format!("{ns_file}.rs")),
                "py" => (idl::gen_py(&file), format!("{ns_file}_gen.py")),
                "ts" => (idl::gen_ts(&file), format!("{ns_file}_gen.ts")),
                "schema" => (idl::gen_schema(&file), format!("{ns_file}.schema.json")),
                other => {
                    eprintln!("unknown language '{other}' (rust|py|ts|schema)");
                    std::process::exit(2);
                }
            };
            let path = format!("{out_dir}/{fname}");
            std::fs::write(&path, content).expect("write");
            println!("{path}");
        }
        other => {
            eprintln!("unknown command '{other}'");
            std::process::exit(2);
        }
    }
}
