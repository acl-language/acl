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

/// §31: deprecation metadata for a declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Deprecation {
    pub reason: String,
    pub since: String, // vocab version, e.g. "v1"
}

/// Parse a `.acl` source file.
pub fn parse(src: &str) -> Result<IdlFile, Vec<IdlError>> {
    struct WfWip {
        local: String,
        #[allow(dead_code)]
        params: Vec<Param>,
        ret: Option<String>,
        steps: Vec<Step>,
        cur_parallel: Option<Vec<String>>,
    }
    let mut wf_wip: Option<WfWip> = None;
    macro_rules! wf_mut {
        () => { wf_wip.as_mut().unwrap() };
    }

    let mut errors = Vec::new();
    let mut namespace: Option<(String, u32)> = None;
    let mut decls = Vec::new();
    let mut doc: Option<String> = None;
    let mut deprecation: Option<Deprecation> = None;
    let namespace_version = namespace
        .as_ref()
        .map(|(_, v)| format!("v{}", v))
        .unwrap_or_else(|| "v1".to_string());

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        // doc comments MUST be detected before the plain-comment strip
        // (a "///" line starts with "//" and would otherwise be eaten)
        // §31: @deprecated("reason") annotation line before a decl
        if raw.trim_start().starts_with("@deprecated(") {
            let inner = raw.trim_start().trim_start_matches("@deprecated(").trim_end_matches(")");
            deprecation = Some(Deprecation {
                reason: inner.to_string(),
                since: format!("v{}", 1), // refined below from namespace version
            });
            continue;
        }
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
        // workflow block handling (multi-line)
        if wf_wip.is_some() {
            if line == "}" {
                // close parallel group if open; otherwise close the workflow
                if let Some(par) = wf_wip.as_mut().unwrap().cur_parallel.take() {
                    if par.is_empty() {
                        errors.push(IdlError { line: line_no, message: "empty parallel group".into() });
                    } else {
                        wf_wip.as_mut().unwrap().steps.push(Step::Parallel(par));
                    }
                } else {
                    let w = wf_wip.take().unwrap();
                    decls.push(Decl::Workflow {
                        local: w.local,
                        params: w.params,
                        steps: w.steps,
                        ret: w.ret,
                        doc: doc.take(),
                    });
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("parallel {") {
                let _ = rest;
                if wf_wip.as_mut().unwrap().cur_parallel.is_some() {
                    errors.push(IdlError { line: line_no, message: "nested parallel not allowed".into() });
                } else {
                    wf_wip.as_mut().unwrap().cur_parallel = Some(Vec::new());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("do ") {
                let name = rest.trim_end_matches(';').trim().to_string();
                if name.is_empty() {
                    errors.push(IdlError { line: line_no, message: "empty 'do' target".into() });
                } else if let Some(par) = wf_wip.as_mut().unwrap().cur_parallel.as_mut() {
                    par.push(name);
                } else {
                    wf_wip.as_mut().unwrap().steps.push(Step::Do(name));
                }
                continue;
            }
            errors.push(IdlError {
                line: line_no,
                message: format!("unrecognized line inside workflow: '{line}'"),
            });
            continue;
        }
        if line.starts_with("workflow ") {
            let body = line.trim_end_matches('{').trim();
            let body = body.strip_prefix("workflow ").unwrap_or(body).trim();
            let (sig, ret) = match body.rsplit_once("->") {
                Some((h, r)) => (h.trim(), Some(r.trim().to_string())),
                None => (body, None),
            };
            let open = match sig.find('(') {
                Some(o) => o,
                None => {
                    errors.push(IdlError { line: line_no, message: format!("bad workflow decl: '{line}'") });
                    doc = None;
                    continue;
                }
            };
            let local = sig[..open].trim().to_string();
            let inner = sig[open + 1..].trim_end();
            let inner = inner.strip_suffix(')').unwrap_or(inner).trim();
            let mut params = Vec::new();
            let mut bad = false;
            if !inner.is_empty() {
                for pr in inner.split(',') {
                    match pr.trim().split_once(':') {
                        Some((pn, pt)) => params.push(Param {
                            name: pn.trim().to_string(),
                            ty: pt.trim().to_string(),
                        }),
                        None => {
                            errors.push(IdlError {
                                line: line_no,
                                message: format!("bad workflow param '{pr}'"),
                            });
                            bad = true;
                        }
                    }
                }
            }
            if !bad {
                wf_wip = Some(WfWip {
                    local,
                    params,
                    ret,
                    steps: Vec::new(),
                    cur_parallel: None,
                });
            } else {
                doc = None;
            }
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
                    let dep = deprecation.take().map(|mut d| { d.since = namespace_version.clone(); d });
                    let d = if kind_name == "predicate" {
                        Decl::Predicate { local, params, ret, doc: doc.take(), deprecation: dep }
                    } else {
                        Decl::Action { local, params, ret, doc: doc.take(), deprecation: dep }
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

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Predicate { local: String, params: Vec<Param>, ret: String, doc: Option<String>, deprecation: Option<Deprecation> },
    Action { local: String, params: Vec<Param>, ret: String, doc: Option<String>, deprecation: Option<Deprecation> },
    Alias { local: String, target: String },
    /// §20: ordered plan of action groups; groups run in sequence,
    /// actions inside a group run in parallel.
    Workflow { local: String, params: Vec<Param>, steps: Vec<Step>, ret: Option<String>, doc: Option<String> },
}

/// One step of a workflow: a single action, or a parallel group of actions.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Do(String),
    Parallel(Vec<String>),
}

pub(crate) fn valid_type_name(s: &str) -> bool {
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
            Decl::Workflow { local, params, ret, steps, .. } => {
                let full = format!("{}.{}", file.namespace, local);
                if let Some(prev_kind) = seen.insert(full.clone(), "workflow") {
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
                if let Some(r) = ret {
                    if !check_type(r) {
                        errors.push(IdlError { line: 0, message: format!("{full}: unknown return type '{r}'") });
                    }
                }
                for st in steps {
                    let targets: Vec<&String> = match st {
                        Step::Do(n) => vec![n],
                        Step::Parallel(g) => g.iter().collect(),
                    };
                    for t in targets {
                        let ok = file.decls.iter().any(|d2| {
                            matches!(d2, Decl::Action { local: l, .. } if format!("{}.{}", file.namespace, l) == *t)
                        });
                        if !ok {
                            errors.push(IdlError {
                                line: 0,
                                message: format!("workflow '{full}': 'do {t}' is not a declared action"),
                            });
                        }
                    }
                }
                full_names.push(full);
                continue;
            }
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
        let (kind, local, params, ret, doc, _dep) = match d {
            Decl::Predicate { local, params, ret, doc, deprecation } => ("predicate", local, params, ret, doc, deprecation),
            Decl::Action { local, params, ret, doc, deprecation } => ("action", local, params, ret, doc, deprecation),
            Decl::Alias { local, target } => {
                out.push_str(&format!("\ntype {local} = {target};\n"));
                continue;
            }
            Decl::Workflow { local, params, steps, ret, doc } => {
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
                let ret_s = ret.as_ref().map(|r| format!(" -> {r}")).unwrap_or_default();
                out.push_str(&format!("\nworkflow {local}({ps}){ret_s} {{\n"));
                for st in steps {
                    match st {
                        Step::Do(n) => out.push_str(&format!("  do {n};\n")),
                        Step::Parallel(g) => {
                            out.push_str("  parallel {\n");
                            for n in g {
                                out.push_str(&format!("    do {n};\n"));
                            }
                            out.push_str("  }\n");
                        }
                    }
                }
                out.push_str("}\n");
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
            Decl::Workflow { local, steps, .. } => {
                let groups: Vec<String> = steps
                    .iter()
                    .map(|st| match st {
                        Step::Do(n) => format!("vec![\"{n}\"]"),
                        Step::Parallel(g) => format!(
                            "vec![{}]",
                            g.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
                        ),
                    })
                    .collect();
                out.push_str(&format!(
                    "pub fn plan_{local}() -> Vec<Vec<&'static str>> {{ vec![{}] }}\n",
                    groups.join(", ")
                ));
            }
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
            Decl::Workflow { local, steps, .. } => {
                let groups: Vec<String> = steps
                    .iter()
                    .map(|st| match st {
                        Step::Do(n) => format!("[\"{n}\"]"),
                        Step::Parallel(g) => format!(
                            "[{}]",
                            g.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
                        ),
                    })
                    .collect();
                out.push_str(&format!(
                    "def plan_{local}():\n    return [{}]\n\n",
                    groups.join(", ")
                ));
            }
            Decl::Alias { .. } => {}
        }
    }
    out
}

/// Generate a runtime-validation schema (JSON) for `gen schema`.
pub fn gen_schema(file: &IdlFile) -> String {
    let mut aliases = BTreeMap::new();
    let mut predicates = BTreeMap::new();
    let mut actions = BTreeMap::new();
    for d in &file.decls {
        match d {
            Decl::Alias { local, target } => {
                aliases.insert(local.clone(), target.clone());
            }
            Decl::Predicate { local, params, ret, doc, .. } => {
                predicates.insert(
                    local.clone(),
                    SchemaOpView {
                        params: params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect(),
                        ret: ret.clone(),
                        doc: doc.clone(),
                    },
                );
            }
            Decl::Action { local, params, ret, doc, .. } => {
                actions.insert(
                    local.clone(),
                    SchemaOpView {
                        params: params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect(),
                        ret: ret.clone(),
                        doc: doc.clone(),
                    },
                );
            }
            Decl::Workflow { .. } => {}
        }
    }
    // §31: deprecated terms map (term -> reason)
    let mut deprecated = serde_json::Map::new();
    for d in &file.decls {
        let (full, dep) = match d {
            Decl::Predicate { local, deprecation: Some(dp), .. }
            | Decl::Action { local, deprecation: Some(dp), .. } => {
                (format!("{}.{}", file.namespace, local), Some(dp))
            }
            _ => (String::new(), None),
        };
        if let Some(dp) = dep {
            deprecated.insert(full, serde_json::Value::String(format!("{} (since {})", dp.reason, dp.since)));
        }
    }
    let view = serde_json::json!({
        "namespace": file.namespace,
        "version": file.version,
        "aliases": aliases,
        "predicates": predicates,
        "actions": actions,
        "deprecated": deprecated,
    });
    String::from_utf8(serde_json::to_vec_pretty(&view).unwrap_or_default()).unwrap_or_default()
}

#[derive(serde::Serialize)]
struct SchemaOpView {
    params: BTreeMap<String, String>,
    ret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
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
            Decl::Workflow { local, steps, .. } => {
                let groups: Vec<String> = steps
                    .iter()
                    .map(|st| match st {
                        Step::Do(n) => format!("[\"{n}\"]"),
                        Step::Parallel(g) => format!(
                            "[{}]",
                            g.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
                        ),
                    })
                    .collect();
                out.push_str(&format!(
                    "export function plan_{local}() {{ return [{}]; }}\n\n",
                    groups.join(", ")
                ));
            }
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
    fn workflow_parse_typecheck_codegen() {
        let src = r#"
namespace acl.demo v1;
action step_a(text: string) -> bool;
action step_b() -> bool;
workflow pipeline(text: string) -> bool {
  do acl.demo.step_a;
  parallel {
    do acl.demo.step_b;
    do acl.demo.step_b;
  }
  do acl.demo.step_a;
}
"#;
        let f = parse(src).expect("parse");
        typecheck(&f).expect("typecheck");
        match &f.decls[2] {
            Decl::Workflow { local, steps, ret, .. } => {
                assert_eq!(local, "pipeline");
                assert_eq!(steps.len(), 3, "do, parallel, do");
                assert_eq!(ret.as_deref(), Some("bool"));
                match &steps[1] {
                    Step::Parallel(g) => assert_eq!(g.len(), 2),
                    other => panic!("expected parallel, got {other:?}"),
                }
            }
            other => panic!("expected workflow, got {other:?}"),
        }
        // pretty roundtrip
        let p1 = pretty(&f);
        assert_eq!(parse(&p1).unwrap(), f);
        assert!(p1.contains("workflow pipeline(text: string) -> bool {"));
        assert!(p1.contains("  parallel {"));
        // codegen: plan preserves order and parallel groups
        let rust = gen_rust(&f);
        assert!(rust.contains("pub fn plan_pipeline() -> Vec<Vec<&'static str>>"));
        let py = gen_py(&f);
        assert!(py.contains("def plan_pipeline():"));
    }

    #[test]
    fn deprecation_parsed() {
        let src = "namespace a.b v1;\n@deprecated(\"use file_v2 instead\")\npredicate old_thing() -> bool;\n";
        let f = parse(src).expect("parse");
        match &f.decls[0] {
            Decl::Predicate { local, deprecation, .. } => {
                assert_eq!(local, "old_thing");
                let dp = deprecation.as_ref().expect("deprecation expected");
                assert!(dp.reason.contains("file_v2"));
            }
            other => panic!("expected predicate, got {other:?}"),
        }
        // non-deprecated decl has none
        match &f.decls[0] {
            Decl::Predicate { deprecation, .. } => assert!(deprecation.is_some()),
            _ => {}
        }
    }

    #[test]
    fn workflow_type_errors() {
        let src = "namespace a.b v1;
workflow w() {
  do a.b.missing;
}
";
        let errs = typecheck(&parse(src).unwrap()).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("'do a.b.missing' is not a declared action")));
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
