//! acl-py — Python bindings for acl-core via PyO3.
//!
//! Exposes the ACL envelope (SPEC.md §4–§6) to Python: build, sign,
//! verify, encode/decode, framing.

use acl_core::{Aid, Body, Envelope, Performative, ACL_VERSION};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

fn to_py_err(e: acl_core::AclError) -> PyErr {
    PyValueError::new_err(e.to_string())
}

// ---------- body conversion ------------------------------------------------

fn py_to_json(v: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    let py = v.py();
    let s: String = py
        .import("json")?
        .call_method1("dumps", (v,))?
        .extract()?;
    Ok(serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))
}

fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyObject {
    py.import("json")
        .and_then(|m| m.call_method1("loads", (v.to_string(),)))
        .map(|o| o.unbind())
        .unwrap_or_else(|_| py.None())
}

fn body_from_py(body: &Bound<'_, PyAny>) -> PyResult<Body> {
    let kind: String = body.get_item("t")?.extract()?;
    let v = body.get_item("v")?;
    let opt_field = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        // missing key -> None (get_item raises KeyError on plain dicts)
        match v.get_item(name) {
            Ok(f) => Ok(if f.is_none() { None } else { Some(f) }),
            Err(e) if e.is_instance_of::<pyo3::exceptions::PyKeyError>(v.py()) => Ok(None),
            Err(e) => Err(e),
        }
    };
    let field = |name: &str| -> PyResult<Bound<'_, PyAny>> {
        opt_field(name)?.ok_or_else(|| PyValueError::new_err(format!("body missing '{name}'")))
    };
    let s = |name: &str| -> PyResult<String> { field(name)?.extract() };
    let opt_s = |name: &str| -> PyResult<Option<String>> {
        opt_field(name)?.map(|f| f.extract()).transpose()
    };
    let opt_json = |name: &str| -> PyResult<Option<serde_json::Value>> {
        Ok(match opt_field(name)? {
            Some(f) => Some(py_to_json(&f)?),
            None => None,
        })
    };
    let json_or_null = |name: &str| -> PyResult<serde_json::Value> {
        Ok(opt_json(name)?.unwrap_or(serde_json::Value::Null))
    };
    match kind.as_str() {
        "Task" => Ok(Body::Task {
            description: s("description")?,
            input: json_or_null("input")?,
            deadline_ms: opt_json("deadline_ms")?.and_then(|x| x.as_u64().map(|x| x as u32)),
            priority: opt_json("priority")?.and_then(|x| x.as_u64().map(|x| x as u8)),
        }),
        "Result" => Ok(Body::Result {
            output: json_or_null("output")?,
            artifacts: vec![],
        }),
        "Text" => Ok(Body::Text { text: s("text")? }),
        "Query" => Ok(Body::Query {
            subject: s("subject")?,
            parameters: opt_json("parameters")?,
        }),
        "Proposal" => Ok(Body::Proposal {
            terms: json_or_null("terms")?,
            price: opt_s("price")?,
            valid_until: opt_json("valid_until")?.and_then(|x| x.as_i64()),
        }),
        "Ref" => Ok(Body::Ref {
            target_msg_id: s("target_msg_id")?,
        }),
        "Progress" => Ok(Body::Progress {
            target_msg_id: s("target_msg_id")?,
            percent: opt_json("percent")?.and_then(|x| x.as_u64().map(|x| x as u8)),
            note: opt_s("note")?,
        }),
        "InputNeeded" => Ok(Body::InputNeeded {
            target_msg_id: s("target_msg_id")?,
            what: s("what")?,
            schema: opt_json("schema")?,
        }),
        "Failure" => Ok(Body::Failure {
            target_msg_id: s("target_msg_id")?,
            code: opt_json("code")?.and_then(|x| x.as_u64().map(|x| x as u16)).unwrap_or(500),
            reason: s("reason")?,
            recoverable: opt_json("recoverable")?.and_then(|x| x.as_bool()).unwrap_or(false),
        }),
        "Error" => Ok(Body::Error {
            code: opt_json("code")?.and_then(|x| x.as_u64().map(|x| x as u16)).unwrap_or(400),
            message: s("message")?,
            details: opt_json("details")?,
        }),
        "Hello" => Ok(Body::Hello {
            agent_card: None,
            card_hash: None,
            nonce: vec![],
        }),
        "Sealed" => Ok(Body::Sealed {
            sender_pub: vec![],
            salt: vec![],
            ciphertext: vec![],
        }),
        "Cl" => {
            let exprs_val = json_or_null("expressions")?;
            let exprs: Vec<acl_core::cl::ClExpr> = serde_json::from_value(exprs_val)
                .map_err(|e| PyValueError::new_err(format!("bad CL expressions: {e}")))?;
            Ok(Body::Cl { expressions: exprs })
        }
        other => Err(PyValueError::new_err(format!("unknown body kind {other}"))),
    }
}

fn body_to_py(py: Python<'_>, b: &Body) -> PyObject {
    let d = PyDict::new(py);
    let st = |v: &String| v.clone().into_pyobject(py).unwrap().unbind().into();
    let opt_st = |v: &Option<String>| v.clone().into_pyobject(py).unwrap().unbind().into();
    let (t, fields): (&str, Vec<(&str, PyObject)>) = match b {
        Body::Task { description, input, deadline_ms, priority } => (
            "Task",
            vec![
                ("description", st(description)),
                ("input", json_to_py(py, input)),
                ("deadline_ms", deadline_ms.map(|x| x.into_pyobject(py).unwrap().unbind().into()).unwrap_or(py.None())),
                ("priority", priority.map(|x| x.into_pyobject(py).unwrap().unbind().into()).unwrap_or(py.None())),
            ],
        ),
        Body::Result { output, .. } => (
            "Result",
            vec![
                ("output", json_to_py(py, output)),
                ("artifacts", Vec::<u8>::new().into_pyobject(py).unwrap().unbind().into()),
            ],
        ),
        Body::Text { text } => ("Text", vec![("text", st(text))]),
        Body::Query { subject, parameters } => (
            "Query",
            vec![
                ("subject", st(subject)),
                ("parameters", parameters.as_ref().map(|x| json_to_py(py, x)).unwrap_or(py.None())),
            ],
        ),
        Body::Proposal { terms, price, valid_until } => (
            "Proposal",
            vec![
                ("terms", json_to_py(py, terms)),
                ("price", opt_st(price)),
                ("valid_until", valid_until.map(|x| x.into_pyobject(py).unwrap().unbind().into()).unwrap_or(py.None())),
            ],
        ),
        Body::Ref { target_msg_id } => ("Ref", vec![("target_msg_id", st(target_msg_id))]),
        Body::Progress { target_msg_id, percent, note } => (
            "Progress",
            vec![
                ("target_msg_id", st(target_msg_id)),
                ("percent", percent.map(|x| x.into_pyobject(py).unwrap().unbind().into()).unwrap_or(py.None())),
                ("note", opt_st(note)),
            ],
        ),
        Body::InputNeeded { target_msg_id, what, schema } => (
            "InputNeeded",
            vec![
                ("target_msg_id", st(target_msg_id)),
                ("what", st(what)),
                ("schema", schema.as_ref().map(|x| json_to_py(py, x)).unwrap_or(py.None())),
            ],
        ),
        Body::Failure { target_msg_id, code, reason, recoverable } => (
            "Failure",
            vec![
                ("target_msg_id", st(target_msg_id)),
                ("code", (*code).into_pyobject(py).unwrap().unbind().into()),
                ("reason", st(reason)),
                ("recoverable", recoverable.into_pyobject(py).unwrap().to_owned().unbind().into()),
            ],
        ),
        Body::Error { code, message, details } => (
            "Error",
            vec![
                ("code", (*code).into_pyobject(py).unwrap().unbind().into()),
                ("message", st(message)),
                ("details", details.as_ref().map(|x| json_to_py(py, x)).unwrap_or(py.None())),
            ],
        ),
        Body::Hello { nonce, .. } => (
            "Hello",
            vec![("nonce", nonce.clone().into_pyobject(py).unwrap().unbind().into())],
        ),
        Body::Delegate { chain, .. } => (
            "Delegate",
            vec![("chain", chain.clone().into_pyobject(py).unwrap().unbind().into())],
        ),
        Body::Sealed { sender_pub, salt, ciphertext } => (
            "Sealed",
            vec![
                ("sender_pub", sender_pub.clone().into_pyobject(py).unwrap().unbind().into()),
                ("salt", salt.clone().into_pyobject(py).unwrap().unbind().into()),
                ("ciphertext", ciphertext.clone().into_pyobject(py).unwrap().unbind().into()),
            ],
        ),
        Body::Cl { expressions } => {
            let val = serde_json::to_value(expressions).unwrap_or(serde_json::Value::Null);
            ("Cl", vec![("expressions", json_to_py(py, &val))])
        }
    };
    d.set_item("t", t).unwrap();
    let vd = PyDict::new(py);
    for (k, val) in fields {
        vd.set_item(k, val).unwrap();
    }
    d.set_item("v", vd).unwrap();
    d.into_any().unbind()
}

fn perf_from_str(name: &str) -> PyResult<Performative> {
    Ok(match name {
        "request" => Performative::Request,
        "inform" => Performative::Inform,
        "query" => Performative::Query,
        "propose" => Performative::Propose,
        "accept" => Performative::Accept,
        "reject" => Performative::Reject,
        "cancel" => Performative::Cancel,
        "progress" => Performative::Progress,
        "input_required" => Performative::InputRequired,
        "failure" => Performative::Failure,
        "delegate" => Performative::Delegate,
        "error" => Performative::Error,
        "hello" => Performative::Hello,
        "bye" => Performative::Bye,
        _ => return Err(PyValueError::new_err(format!("unknown performative {name}"))),
    })
}

fn perf_name(p: Performative) -> &'static str {
    match p {
        Performative::Request => "request",
        Performative::Inform => "inform",
        Performative::Query => "query",
        Performative::Propose => "propose",
        Performative::Accept => "accept",
        Performative::Reject => "reject",
        Performative::Cancel => "cancel",
        Performative::Progress => "progress",
        Performative::InputRequired => "input_required",
        Performative::Failure => "failure",
        Performative::Delegate => "delegate",
        Performative::Error => "error",
        Performative::Hello => "hello",
        Performative::Bye => "bye",
    }
}

// ---------- classes --------------------------------------------------------

/// Native Ed25519 key (32-byte seed).
#[pyclass]
struct AclKey {
    inner: ed25519_dalek::SigningKey,
}

#[pymethods]
impl AclKey {
    #[new]
    fn new() -> Self {
        use rand::Rng;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        AclKey {
            inner: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    #[staticmethod]
    fn from_seed(seed: Vec<u8>) -> PyResult<Self> {
        if seed.len() != 32 {
            return Err(PyValueError::new_err("seed must be 32 bytes"));
        }
        Ok(AclKey {
            inner: ed25519_dalek::SigningKey::from_bytes(
                &<[u8; 32]>::try_from(seed.as_slice()).unwrap(),
            ),
        })
    }

    fn public_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.verifying_key().to_bytes())
    }
}

/// The ACL envelope (SPEC.md §4).
#[pyclass]
struct AclEnvelope {
    inner: acl_core::Envelope,
}

#[pymethods]
impl AclEnvelope {
    #[new]
    #[pyo3(signature = (from_aid, to_aid, performative, body, in_reply_to=None, reply_with=None))]
    fn new(
        from_aid: String,
        to_aid: String,
        performative: String,
        body: &Bound<'_, PyAny>,
        in_reply_to: Option<String>,
        reply_with: Option<String>,
    ) -> PyResult<Self> {
        Ok(AclEnvelope {
            inner: Envelope {
                acl: ACL_VERSION,
                msg_id: acl_core::new_msg_id(),
                in_reply_to,
                from: Aid::new(from_aid),
                to: Aid::new(to_aid),
                ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                ttl: acl_core::DEFAULT_TTL_MS,
                reply_with,
                hop_count: 0,
                performative: perf_from_str(&performative)?,
                body: body_from_py(body)?,
                sig: None,
                rational_effect: None,
                feasibility_precondition: None,
            },
        })
    }

    fn sign(&mut self, key: &AclKey) -> PyResult<()> {
        self.inner.sign(&key.inner).map_err(to_py_err)
    }

    fn verify(&self, pubkey: Vec<u8>) -> PyResult<bool> {
        let vk = ed25519_dalek::VerifyingKey::from_bytes(
            &<[u8; 32]>::try_from(pubkey.as_slice())
                .map_err(|_| PyValueError::new_err("pubkey must be 32 bytes"))?,
        )
        .map_err(|_| PyValueError::new_err("bad key"))?;
        Ok(self.inner.verify(&vk).map_err(to_py_err)?)
    }

    fn encode<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = self.inner.encode().map_err(to_py_err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    #[staticmethod]
    fn decode(data: Vec<u8>) -> PyResult<AclEnvelope> {
        Ok(AclEnvelope {
            inner: Envelope::decode(&data).map_err(to_py_err)?,
        })
    }

    #[getter]
    fn msg_id(&self) -> &str {
        &self.inner.msg_id
    }
    #[getter]
    fn from_aid(&self) -> &str {
        &self.inner.from.0
    }
    #[getter]
    fn to_aid(&self) -> &str {
        &self.inner.to.0
    }
    #[getter]
    fn performative(&self) -> &'static str {
        perf_name(self.inner.performative)
    }
    #[getter]
    fn in_reply_to(&self) -> Option<String> {
        self.inner.in_reply_to.clone()
    }
    #[getter]
    fn body(&self) -> PyObject {
        Python::with_gil(|py| body_to_py(py, &self.inner.body))
    }

    fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.inner.is_expired(now)
    }
}

// ---------- framing --------------------------------------------------------

/// Frame a payload: 4-byte BE length + payload (spec §3).
#[pyfunction]
fn write_frame<'py>(py: Python<'py>, payload: Vec<u8>) -> Bound<'py, PyBytes> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    PyBytes::new(py, &out)
}

/// Unframe: returns the payload or None if fewer than 4 bytes.
#[pyfunction]
fn read_frame<'py>(py: Python<'py>, stream: Vec<u8>) -> PyResult<Option<Bound<'py, PyBytes>>> {
    if stream.len() < 4 {
        return Ok(None);
    }
    let n = u32::from_be_bytes([stream[0], stream[1], stream[2], stream[3]]) as usize;
    if stream.len() < 4 + n {
        return Err(PyValueError::new_err("incomplete frame"));
    }
    Ok(Some(PyBytes::new(py, &stream[4..4 + n])))
}

#[pyfunction]
fn new_msg_id() -> String {
    acl_core::new_msg_id()
}

#[pymodule]
fn acl_py(py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("ACL_VERSION", ACL_VERSION)?;
    m.add_class::<AclEnvelope>()?;
    m.add_class::<AclKey>()?;
    m.add_function(wrap_pyfunction!(write_frame, py)?)?;
    m.add_function(wrap_pyfunction!(read_frame, py)?)?;
    m.add_function(wrap_pyfunction!(new_msg_id, py)?)?;
    Ok(())
}
