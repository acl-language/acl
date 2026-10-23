//! aclc — the ACL language compiler (spec §16 toolchain).
//!
//!   aclc check <file.acl>...          parse + typecheck (exit 0/1)
//!   aclc lint  <file.acl>...          check + style rules (doc on every decl)
//!   aclc fmt   <file.acl>...          canonical pretty-print in place
//!   aclc gen   <rust|py|ts> <file.acl> <out_dir>
//!   aclc names <file.acl>...          print fully-qualified names

use acl_core::idl::{self, IdlFile};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: aclc check|lint|fmt|gen|names|protocol <args>");
        std::process::exit(2);
    }
    let cmd = args[1].as_str();
    let files = &args[2..];

    match cmd {
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
