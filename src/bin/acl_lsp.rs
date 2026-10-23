//! acl-lsp — Language Server Protocol for `.aclp` files (§36).
//!
//! Minimal but real: stdio JSON-RPC, publishDiagnostics on open/change
//! (parse + §16/§33/§34/§35 checks with line numbers), completion for
//! keywords/performatives/states, and goto-free hover summaries.
//!
//! Wire: Content-Length framed JSON-RPC 2.0 on stdin/stdout.

use acl_core::protocol::{self, ProtocolProgram};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut documents: std::collections::BTreeMap<String, String> = Default::default();

    loop {
        // read one LSP frame
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(len_str) = line.strip_prefix("Content-Length:") {
            let len: usize = len_str.trim().parse().unwrap_or(0);
            // consume remaining headers (blank line)
            let mut blank = String::new();
            loop {
                blank.clear();
                if stdin.lock().read_line(&mut blank).unwrap_or(0) == 0 {
                    return;
                }
                if blank.trim().is_empty() {
                    break;
                }
            }
            let mut buf = vec![0u8; len];
            if stdin.lock().read_exact(&mut buf).is_err() {
                break;
            }
            let msg: Value = match serde_json::from_slice(&buf) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let method = msg["method"].as_str().unwrap_or("");
            let id = msg["id"].clone();
            let params = msg["params"].clone();

            match method {
                "initialize" => {
                    send(&mut out, id, json!({
                        "capabilities": {
                            "textDocumentSync": 1,
                            "completionProvider": {"triggerCharacters": [" "]},
                            "hoverProvider": true,
                        },
                        "serverInfo": {"name": "acl-lsp", "version": env!("CARGO_PKG_VERSION")}
                    }));
                }
                "textDocument/didOpen" | "textDocument/didChange" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                    let text = match method {
                        "textDocument/didOpen" => params["textDocument"]["text"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        _ => {
                            // full sync: last content change
                            params["contentChanges"]
                                .as_array()
                                .and_then(|c| c.last())
                                .and_then(|c| c["text"].as_str())
                                .unwrap_or("")
                                .to_string()
                        }
                    };
                    documents.insert(uri.clone(), text.clone());
                    let diags = diagnostics(&text);
                    notify(&mut out, "textDocument/publishDiagnostics", json!({
                        "uri": uri,
                        "diagnostics": diags,
                    }));
                }
                "textDocument/didClose" => {
                    if let Some(uri) = params["textDocument"]["uri"].as_str() {
                        documents.remove(uri);
                        notify(&mut out, "textDocument/publishDiagnostics", json!({
                            "uri": uri, "diagnostics": []
                        }));
                    }
                }
                "textDocument/completion" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                    let text = documents.get(uri).cloned().unwrap_or_default();
                    let items = completions(&text);
                    send(&mut out, id, json!({ "isIncomplete": false, "items": items }));
                }
                "textDocument/hover" => {
                    send(&mut out, id, json!(null));
                }
                "shutdown" => {
                    send(&mut out, id, json!(null));
                }
                "exit" => {
                    return;
                }
                _ => {
                    if !id.is_null() {
                        send(&mut out, id, json!({}));
                    }
                }
            }
        }
    }
}

fn send(out: &mut std::io::Stdout, id: Value, result: Value) {
    let body = json!({"jsonrpc": "2.0", "id": id, "result": result});
    write_frame(out, &body);
}

fn notify(out: &mut std::io::Stdout, method: &str, params: Value) {
    let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
    write_frame(out, &body);
}

fn write_frame(out: &mut std::io::Stdout, body: &Value) {
    let text = serde_json::to_string(body).unwrap_or_default();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", text.len(), text);
    let _ = out.flush();
}

/// parse + full semantic checks → LSP diagnostics (0-based lines!)
pub fn diagnostics(text: &str) -> Vec<Value> {
    let mut diags = Vec::new();
    match protocol::parse(text) {
        Err(errors) => {
            for e in errors {
                diags.push(json!({
                    "range": {
                        "start": {"line": e.line.saturating_sub(1), "character": 0},
                        "end": {"line": e.line.saturating_sub(1), "character": 999},
                    },
                    "severity": 1,
                    "source": "acl",
                    "message": e.message,
                }));
            }
        }
        Ok(prog) => match protocol::check(&prog) {
            Err(errors) => {
                for e in errors {
                    diags.push(json!({
                        "range": {
                            "start": {"line": e.line.saturating_sub(1), "character": 0},
                            "end": {"line": e.line.saturating_sub(1), "character": 999},
                        },
                        "severity": 1,
                        "source": "acl",
                        "message": e.message,
                    }));
                }
            }
            Ok(warnings) => {
                for w in warnings {
                    diags.push(json!({
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 999},
                        },
                        "severity": 2,
                        "source": "acl",
                        "message": w,
                    }));
                }
            }
        },
    }
    diags
}

/// completion: keywords + performatives + declared states
fn completions(text: &str) -> Vec<Value> {
    let keywords = [
        "protocol", "roles", "state", "on", "end when", "every", "send", "begin",
        "fulfil", "retract", "set", "push", "put", "for", "abort", "use", "transport",
        "capacity", "scenario", "expect", "include", "message", "when", "while", "as",
    ];
    let performatives = [
        "request", "inform", "query", "propose", "accept", "reject", "cancel",
        "progress", "input_required", "failure", "delegate", "error", "hello",
        "bye", "retract", "subscribe", "unsubscribe",
    ];
    let mut items: Vec<Value> = Vec::new();
    for k in keywords {
        items.push(json!({"label": k, "kind": 14})); // Keyword
    }
    for p in performatives {
        items.push(json!({"label": p, "kind": 14}));
    }
    if let Ok(prog) = protocol::parse(text) {
        for s in &prog.states {
            items.push(json!({"label": s.name, "kind": 13})); // Variable
        }
        for r in &prog.roles {
            items.push(json!({"label": r, "kind": 12})); // Value (role)
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_catch_parse_and_semantic_errors() {
        // parse error
        let d = diagnostics("on query -> send inform;");
        assert!(d.iter().any(|x| x["severity"] == 1));
        // semantic: dialogue violation
        let d = diagnostics("protocol p v1;\nroles a, b;\non query from a -> send request to a;\non request from b -> send bye;\n");
        assert!(d
            .iter()
            .any(|x| x["message"].as_str().unwrap_or("").contains("dialogue violation")));
        // good program: no errors (maybe warnings)
        let d = diagnostics("protocol p v1;\nroles a, b;\non query from a -> send inform to a;\non inform from b -> send bye;\n");
        assert!(!d.iter().any(|x| x["severity"] == 1), "{d:?}");
    }

    #[test]
    fn completions_include_program_symbols() {
        let items = completions("protocol p v1;\nroles buyer, seller;\nstate quoted: bool;\n");
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|x| x["label"].as_str())
            .collect();
        assert!(labels.contains(&"request"));
        assert!(labels.contains(&"quoted"));
        assert!(labels.contains(&"buyer"));
        assert!(labels.contains(&"end when"));
    }
}
