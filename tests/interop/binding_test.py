#!/usr/bin/env python3
"""acl-py binding test: build, sign, verify, encode/decode, framing, tamper check."""

import sys, os, time, secrets
sys.path.insert(0, "/home/guilherme/apps/acl/acl-py/target/debug")
import acl_py

print("ACL_VERSION =", acl_py.ACL_VERSION)

key = acl_py.AclKey()
pub = key.public_bytes()

# 1. build + sign a request
env = acl_py.AclEnvelope(
    "local:pyclient:bind", "did:example:target", "request",
    {"t": "Task", "v": {"description": "bind-test",
                        "input": {"text": "pyo3"}, "deadline_ms": 5000}},
)
print("[1] msg_id =", env.msg_id, "| perf =", env.performative)
env.sign(key)
raw = env.encode()
print("[1] encoded bytes:", len(raw))

# 2. decode + verify round-trip
back = acl_py.AclEnvelope.decode(raw)
assert back.msg_id == env.msg_id
assert back.verify(pub), "verify failed"
assert back.body["t"] == "Task"
assert back.body["v"]["input"]["text"] == "pyo3"
print("[2] decode+verify OK, body:", back.body["v"]["input"])

# 3. tamper → verify must fail
tampered = acl_py.AclEnvelope.decode(raw)
# can't mutate via binding; decode same bytes with different key → must fail
wrong = acl_py.AclKey()
assert not tampered.verify(wrong.public_bytes()), "tamper check failed"
print("[3] wrong key correctly rejected")

# 4. framing
framed = acl_py.write_frame(raw)
payload = acl_py.read_frame(bytes(framed))
assert bytes(payload) == raw
print("[4] framing round-trip OK, frame =", len(framed), "bytes")

# 5. cross-check against the independent codec signature rule:
#    sign with acl_py, verify with py_codec.verify_sig (pure-python canonical rule)
sys.path.insert(0, "/home/guilherme/apps/acl/tests/interop")
import msgpack
from py_codec import key_of
env2 = acl_py.AclEnvelope(
    "local:a:b", "did:x:y", "inform",
    {"t": "Text", "v": {"text": "hello from rust binding"}},
)
env2.sign(key)
decoded = msgpack.unpackb(bytes(env2.encode()), raw=False)
ok = __import__("py_codec").verify_sig(decoded, pub)
assert ok, "py_codec could not verify acl_py signature"
print("[5] py_codec (SPEC.md-only impl) verifies acl_py signature: True")

print()
print("=== ACL-PY BINDING PASS: 5/5 ===")
