//! IDL — Interface Definition Language for ACL vocabularies (spec §18).
//!
//! `.acl` files declare namespaced predicates and typed actions; `aclc`
//! parses, type-checks, pretty-prints and generates code stubs.
//!
//! Grammar (line-based):
//! ```text
//! namespace fs v1;
//! type Path = string;
//! /// doc comment attached to next decl
//! predicate file_exists(path: Path) -> bool;
//! action text_reverse(text: string) -> string;
//! ```
//! Full name of a decl = `<namespace>.<local>` (e.g. `fs.file_exists`).

use crate::cl::CommitmentStatus;
use std::collections::BTreeMap;

pub const PRIMITIVES: &[&str] = &["string", "int", "float", "bool", "bytes", "timestamp", "any"];

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdlFile {
    pub namespace: String,
    pub version: u32,
    pub decls: Vec<Decl>,
}

#[derive(Debug)]
pub struct IdlError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for IdlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Parse a `.acl` source file.
pub fn parse(src: &str) -> Result<IdlFile, Vec<IdlError>> {
    let mut errors = Vec::new();
    let mut namespace: Option<(String, u32)> = None;
    let mut decls = Vec::new();
    let mut doc: Option<String> = None;

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        // doc comments MUST be detected before the plain-comment strip
        // (a "///" line starts with "//" and would otherwise be eaten)
        if raw.trim_start().starts_with("///") {
            let text = raw.trim_start()[3..].trim().to_string();
            doc = Some(match doc {
                Some(d) => format!("{d}\n{text}"),
                None => text,
            });
            continue;
        }
        let line = match raw.find("//") {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("///") {
            let text = rest.trim().to_string();
            doc = Some(match doc {
                Some(d) => format!("{d}\n{text}"),
                None => text,
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("namespace ") {
            let body = rest.trim_end_matches(';').trim();
            let mut parts = body.split_whitespace();
            let ns = parts.next().unwrap_or_default().to_string();
            let ver = parts
                .next()
                .and_then(|v| v.strip_prefix('v'))
                .and_then(|v| v.parse().ok());
            if ns.is_empty() || !ns.contains('.') || ver.is_none() || parts.next().is_some() {
                errors.push(IdlError { line: line_no, message: format!("bad namespace decl: '{line}' (want 'namespace a.b v1;')") });
                continue;
            }
            if namespace.is_some() {
                errors.push(IdlError { line: line_no, message: "duplicate namespace declaration".into() });
                continue;
            }
            namespace = Some((ns, ver.unwrap()));
            doc = None; // doc before namespace is ignored
            continue;
        }
        if let Some(rest) = line.strip_prefix("type ") {
            let body = rest.trim_end_matches(';').trim();
            match body.split_once('=') {
                Some((alias, ty)) => {
                    // aliases register as decls for typecheck; represented as Predicate-less
                    let alias = alias.trim();
                    let ty = ty.trim();
                    if !valid_type_name(alias) {
                        errors.push(IdlError { line: line_no, message: format!("bad alias name '{alias}'") });
                    }
                    // alias recorded via decls for checking; stored as Action-like marker
                    decls.push(Decl::Alias { local: alias.to_string(), target: ty.to_string() });
                }
                None => errors.push(IdlError { line: line_no, message: format!("bad type decl: '{line}'") }),
            }
            doc = None;
            continue;
        }
        let kind = if line.starts_with("predicate ") {
            Some(("predicate", "predicate "))
        } else if line.starts_with("action ") {
            Some(("action", "action "))
        } else {
            None
        };
        if let Some((kind_name, prefix)) = kind {
            match parse_signature(&line[prefix.len()..], line_no) {
                Ok((local, params, ret)) => {
                    let d = if kind_name == "predicate" {
                        Decl::Predicate { local, params, ret, doc: doc.take() }
                    } else {
                        Decl::Action { local, params, ret, doc: doc.take() }
                    };
                    decls.push(d);
                    continue;
                }
                Err(e) => {
                    errors.push(e);
                    doc = None;
                    continue;
                }
            }
        }
        errors.push(IdlError { line: line_no, message: format!("unrecognized line: '{line}'") });
        doc = None;
    }

    match namespace {
        None => errors.push(IdlError { line: 0, message: "missing namespace declaration".into() }),
        Some((ns, v)) => {
            if !errors.is_empty() {
                return Err(errors);
            }
            return Ok(IdlFile { namespace: ns, version: v, decls });
        }
    }
    Err(errors)
}

#[allow(non_exhaustive)]
#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Predicate { local: String, params: Vec<Param>, ret: String, doc: Option<String> },
    Action { local: String, params: Vec<Param>, ret: String, doc: Option<String> },
    Alias { local: String, target: String },
}

fn valid_type_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse `name(a: T, b: U) -> R` (no kind prefix).
fn parse_signature(body: &str, line_no: usize) -> Result<(String, Vec<Param>, String), IdlError> {
    let bad = |m: String| IdlError { line: line_no, message: m };
    let (head, ret) = body
        .split_once("->")
        .ok_or_else(|| bad(format!("missing '->' in: {body}")))?;
    let ret = ret.trim().trim_end_matches(';').trim().to_string();
    let open = head.find('(').ok_or_else(|| bad("missing '('".into()))?;
    let name = head[..open].trim().to_string();
    if !name.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false)
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(bad(format!("bad decl name '{name}'")));
    }
    // strip exactly ONE closing paren (trim_end_matches would eat the
    // param-list paren of single-param decls: "(path: Path)" -> "path: Path)")
    let inner_raw = head[open + 1..].trim_end();
    let inner = inner_raw.strip_suffix(')').unwrap_or(inner_raw).trim();
    let mut params = Vec::new();
    if !inner.is_empty() {
        for p in inner.split(',') {
            let p = p.trim();
            let (pname, pty) = p
                .split_once(':')
                .ok_or_else(|| bad(format!("bad param '{p}' (want 'name: Type')")))?;
            params.push(Param { name: pname.trim().to_string(), ty: pty.trim().to_string() });
        }
    }
    Ok((name, params, ret))
}

/// Type-check: all referenced types must be primitives or declared aliases;
/// full names must be unique; predicate full name must not collide with
/// action full name. Returns fully-qualified names.
pub fn typecheck(file: &IdlFile) -> Result<Vec<String>, Vec<IdlError>> {
    let mut errors = Vec::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    for d in &file.decls {
        if let Decl::Alias { local, target } = d {
            if aliases.insert(local.clone(), target.clone()).is_some() {
                errors.push(IdlError { line: 0, message: format!("duplicate alias '{local}'") });
            }
        }
    }
    let check_type = |ty: &str| aliases.contains_key(ty) || PRIMITIVES.contains(&ty);
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    let mut full_names = Vec::new();
    for (idx, d) in file.decls.iter().enumerate() {
        let (local, params, ret, is_action) = match d {
            Decl::Predicate { local, params, ret, .. } => (local, params, ret, false),
            Decl::Action { local, params, ret, .. } => (local, params, ret, true),
            Decl::Alias { .. } => continue,
        };
        let full = format!("{}.{}", file.namespace, local);
        if let Some(prev_kind) = seen.insert(full.clone(), if is_action { "action" } else { "predicate" }) {
            errors.push(IdlError {
                line: 0,
                message: format!("duplicate full name '{full}' (already a {prev_kind})"),
            });
        }
        for p in params {
            if !check_type(&p.ty) {
                errors.push(IdlError {
                    line: 0,
                    message: format!("{full}: unknown type '{}' for param '{}'", p.ty, p.name),
                });
            }
        }
        if !check_type(ret) {
            errors.push(IdlError { line: 0, message: format!("{full}: unknown return type '{ret}'") });
        }
        full_names.push(full);
        let _ = idx;
    }
    if errors.is_empty() {
        Ok(full_names)
    } else {
        Err(errors)
    }
}

/// Canonical pretty-print (also the human-readable surface syntax).
/// Declarations keep file order; params aligned; doc comments preserved.
pub fn pretty(file: &IdlFile) -> String {
    let mut out = format!("namespace {} v{};\n", file.namespace, file.version);
    let mut current_doc: Option<String> = None;
    for d in &file.decls {
        let (kind, local, params, ret, doc) = match d {
            Decl::Predicate { local, params, ret, doc } => ("predicate", local, params, ret, doc),
            Decl::Action { local, params, ret, doc } => ("action", local, params, ret, doc),
            Decl::Alias { local, target } => {
                out.push_str(&format!("\ntype {local} = {target};\n"));
                continue;
            }
        };
        if doc.is_some() && doc != &current_doc {
            out.push('\n');
            for l in doc.as_ref().unwrap().lines() {
                out.push_str(&format!("/// {l}\n"));
            }
        }
        current_doc = doc.clone();
        let ps = params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("{kind} {local}({ps}) -> {ret};\n"));
    }
    out
}

/// Generate Rust builder stubs for a vocabulary.
pub fn gen_rust(file: &IdlFile) -> String {
    let mut out = format!(
        "// Generated by aclc from namespace {} v{} — do not edit.\nuse acl_core::cl::{{ClExpr, ClAction, CommitmentStatus}};\npub const NAMESPACE: &str = \"{}\";\npub const VERSION: u32 = {};\n\n",
        file.namespace, file.version, file.namespace, file.version
    );
    for d in &file.decls {
        match d {
            Decl::Predicate { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let args = params
                    .iter()
                    .map(|p| format!("(\"{}\", serde_json::json!({})),", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push_str(&format!(
                    "pub fn fact_{local}({}) -> ClExpr {{ ClExpr::fact(\"{full}\", serde_json::json!([{}])) }}\n",
                    params.iter().map(|p| format!("{}: serde_json::Value", p.name)).collect::<Vec<_>>().join(", "),
                    args
                ));
            }
            Decl::Action { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let params_obj = params
                    .iter()
                    .map(|p| format!("\"{}\": serde_json::json!({})", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "pub fn action_{local}({}) -> ClExpr {{ ClExpr::Action {{ name: \"{full}\".into(), params: serde_json::json!({{{params_obj}}}), result_schema: None }} }}\n",
                    params.iter().map(|p| format!("{}: serde_json::Value", p.name)).collect::<Vec<_>>().join(", ")
                ));
            }
            Decl::Alias { .. } => {}
        }
    }
    let _ = CommitmentStatus::Proposed;
    out
}

/// Generate Python builder stubs.
pub fn gen_py(file: &IdlFile) -> String {
    let mut out = format!(
        "# Generated by aclc from namespace {} v{} — do not edit.\nNAMESPACE = \"{}\"\nVERSION = {}\n\n",
        file.namespace, file.version, file.namespace, file.version
    );
    for d in &file.decls {
        match d {
            Decl::Predicate { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let sig = params.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ");
                let args = params
                    .iter()
                    .map(|p| format!("\"{}\": {}", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "def fact_{local}({sig}):\n    return {{\"op\": \"fact\", \"predicate\": \"{full}\", \"args\": {{{args}}}, \"truth\": True}}\n\n"
                ));
            }
            Decl::Action { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let sig = params.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ");
                let args = params
                    .iter()
                    .map(|p| format!("\"{}\": {}", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "def action_{local}({sig}):\n    return {{\"op\": \"action\", \"name\": \"{full}\", \"params\": {{{args}}}}}\n\n"
                ));
            }
            Decl::Alias { .. } => {}
        }
    }
    out
}

/// Generate TypeScript builder stubs.
pub fn gen_ts(file: &IdlFile) -> String {
    let mut out = format!(
        "// Generated by aclc from namespace {} v{} — do not edit.\nexport const NAMESPACE = \"{}\";\nexport const VERSION = {};\n\n",
        file.namespace, file.version, file.namespace, file.version
    );
    for d in &file.decls {
        match d {
            Decl::Predicate { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let sig = params.iter().map(|p| format!("{}: unknown", p.name)).collect::<Vec<_>>().join(", ");
                let args = params
                    .iter()
                    .map(|p| format!("\"{}\": {}", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "export function fact_{local}({sig}) {{ return {{ op: \"fact\", predicate: \"{full}\", args: {{{args}}}, truth: true }}; }}\n\n"
                ));
            }
            Decl::Action { local, params, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                let sig = params.iter().map(|p| format!("{}: unknown", p.name)).collect::<Vec<_>>().join(", ");
                let args = params
                    .iter()
                    .map(|p| format!("\"{}\": {}", p.name, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "export function action_{local}({sig}) {{ return {{ op: \"action\", name: \"{full}\", params: {{{args}}} }}; }}\n\n"
                ));
            }
            Decl::Alias { .. } => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
namespace acl.std.fs v1;

/// Path on a local filesystem.
type Path = string;

/// Check whether a path exists on the agent's filesystem.
predicate file_exists(path: Path) -> bool;
predicate file_size(path: Path) -> int;
action text_reverse(text: string) -> string;
"#;

    #[test]
    fn parse_and_typecheck_sample() {
        let f = parse(SAMPLE).expect("parse");
        assert_eq!(f.namespace, "acl.std.fs");
        assert_eq!(f.version, 1);
        let names = typecheck(&f).expect("typecheck");
        assert_eq!(names, vec!["acl.std.fs.file_exists", "acl.std.fs.file_size", "acl.std.fs.text_reverse"]);
        // doc attached
        match &f.decls[1] {
            Decl::Predicate { doc, .. } => assert!(doc.as_deref().unwrap().contains("exists on the agent's filesystem")),
            _ => panic!("expected predicate"),
        }
    }

    #[test]
    fn type_errors() {
        let src = "namespace a.b v1;\npredicate f(x: UnknownType) -> bool;\n";
        let errs = typecheck(&parse(src).unwrap()).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("unknown type")));
        // duplicate full name across kinds
        let src2 = "namespace a.b v1;\npredicate f() -> bool;\naction f() -> bool;\n";
        let errs2 = typecheck(&parse(src2).unwrap()).unwrap_err();
        assert!(errs2.iter().any(|e| e.message.contains("duplicate full name")));
    }

    #[test]
    fn parse_errors_have_lines() {
        let src = "namespace a.b v9;\ngarbage line here;\n";
        let errs = parse(src).unwrap_err();
        assert_eq!(errs[0].line, 2);
    }

    #[test]
    fn pretty_is_roundtrip_stable() {
        let f = parse(SAMPLE).unwrap();
        let pretty1 = pretty(&f);
        let f2 = parse(&pretty1).unwrap();
        assert_eq!(f, f2);
        assert_eq!(pretty(&f2), pretty1, "fmt must be idempotent");
        assert!(pretty1.contains("/// Check whether a path exists"));
    }

    #[test]
    fn codegen_contains_builders() {
        let f = parse(SAMPLE).unwrap();
        let rust = gen_rust(&f);
        assert!(rust.contains("pub fn fact_file_exists(path: serde_json::Value)"));
        assert!(rust.contains("pub fn action_text_reverse(text: serde_json::Value)"));
        assert!(rust.contains("NAMESPACE: &str = \"acl.std.fs\""));
        let py = gen_py(&f);
        assert!(py.contains("def fact_file_exists(path):"));
        let ts = gen_ts(&f);
        assert!(ts.contains("export function action_text_reverse(text: unknown)"));
    }
}
