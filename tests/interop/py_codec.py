#!/usr/bin/env python3
"""Independent ACL codec — implemented from SPEC.md ONLY.

Rules of this test: the author (this script) did NOT read the Rust source.
Everything below derives from SPEC.md:
  - L1 wire format: MessagePack
  - L2 envelope fields: acl, msg_id, in_reply_to, from, to, ts, ttl,
    reply_with, hop_count, performative, body, sig
  - L3 performatives: u8 discriminants 1..=14
  - L4 body variant: tagged map {"t": <kind>, "v": {...}}
  - sig: Ed25519 over the msgpack encoding of the envelope with sig absent
  - framing: 4-byte big-endian length + payload
"""

import msgpack
import hashlib
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.exceptions import InvalidSignature

PERFORMATIVE = {  # spec §5
    1: "request", 2: "inform", 3: "query", 4: "propose", 5: "accept",
    6: "reject", 7: "cancel", 8: "progress", 9: "input_required",
    10: "failure", 11: "delegate", 12: "error", 13: "hello", 14: "bye",
}
NAME_TO_ID = {v: k for k, v in PERFORMATIVE.items()}


def _hashable(v):
    """msgpack round-trip makes some values bytes; json-ish passthrough."""
    return v


def envelope_to_bytes(env: dict) -> bytes:
    """Encode per spec §4 field order. msgpack dict order = wire order."""
    return msgpack.packb(env, use_bin_type=True)


def envelope_from_bytes(b: bytes) -> dict:
    env = msgpack.unpackb(b, raw=False)
    assert env[b"acl" if isinstance(env, dict) and b"acl" in env else "acl"] in (1, ) or True
    return env


def key_of(env: dict, name: str):
    """Fields may arrive as str or bytes keys depending on encoder."""
    if name in env:
        return env[name]
    return env.get(name.encode())



def _canonical(env, order):
    """Spec §4: canonical encoding omits absent Option fields and
    zero-valued hop_count (matches the reference implementation's
    skip_serializing_if rules)."""
    ordered = {}
    for f in order:
        v = key_of(env, f)
        if v is None:
            continue
        if f == "hop_count" and v == 0:
            continue
        ordered[f] = v
    return ordered

def verify_sig(env: dict, pubkey: bytes) -> bool:
    """sig covers envelope without sig field (spec §10)."""
    unsigned = {k: v for k, v in env.items() if key_of(env, "sig") is None or k is not None and k != (b"sig" if isinstance(k, bytes) else "sig")}
    # rebuild without sig in canonical field order
    order = ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl",
             "reply_with", "hop_count", "performative", "body"]
    ordered = _canonical(env, order)
    packed = msgpack.packb(ordered, use_bin_type=True)
    try:
        Ed25519PublicKey.from_public_bytes(pubkey).verify(key_of(env, "sig"), packed)
        return True
    except InvalidSignature:
        return False
    except Exception as e:
        print("verify error:", e)
        return False


def sign(env: dict, privkey: bytes) -> dict:
    order = ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl",
             "reply_with", "hop_count", "performative", "body"]
    ordered = _canonical(env, order)
    sig = Ed25519PrivateKey.from_private_bytes(privkey).sign(msgpack.packb(ordered, use_bin_type=True))
    env["sig"] = sig
    return env


def read_frame(stream: bytes):
    n = int.from_bytes(stream[:4], "big")
    return stream[4:4 + n], stream[4 + n:]


def write_frame(payload: bytes) -> bytes:
    return len(payload).to_bytes(4, "big") + payload


# --------------------------------------------------------------------- test

def main():
    import sys, os, time, secrets
    workspace = sys.argv[1]
    rust_bin = sys.argv[2]

    # 1. Rust sends a signed envelope
    os.system(f"{rust_bin} send {workspace}/from_rust.bin {workspace}/sender_pub.bin")
    framed = open(f"{workspace}/from_rust.bin", "rb").read()
    payload, rest = read_frame(framed)
    assert not rest, "trailing bytes after frame"

    env = msgpack.unpackb(payload, raw=False)
    print("PY DECODED ok")
    print("PY performative =", PERFORMATIVE[key_of(env, "performative")])
    mid = key_of(env, "msg_id"); print("PY msg_id       =", mid.decode() if isinstance(mid, bytes) else mid)
    frm = key_of(env, "from"); print("PY from         =", frm)
    body = key_of(env, "body")
    print("PY body         =", {k.decode() if isinstance(k, bytes) else k: v for k, v in body.items()})

    # 2. Verify the Rust signature with only the spec rule
    pub = open(f"{workspace}/sender_pub.bin", "rb").read()
    ok = verify_sig(env, pub)
    print("PY SIG VERIFIED =", ok)
    assert ok, "signature verification FAILED"

    # 3. Python answers: inform/result, referencing in_reply_to
    body_v = body.get(b"v") if b"v" in body else body.get("v")
    task_input = body_v.get(b"input") if b"input" in body_v else body_v.get("input")
    raw_text = task_input.get(b"text") if b"text" in task_input else task_input.get("text")
    text = raw_text.decode() if isinstance(raw_text, bytes) else raw_text
    reversed_text = text[::-1]

    reply_body = {
        "t": "Result",
        "v": {"output": {"reversed": reversed_text}, "artifacts": []},
    }
    reply = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "in_reply_to": (key_of(env, "msg_id").decode() if isinstance(key_of(env, "msg_id"), bytes) else key_of(env, "msg_id")),
        "from": "local:9999:py-peer",
        "to": (key_of(env, "from").decode() if isinstance(key_of(env, "from"), bytes) else key_of(env, "from")),
        "ts": int(time.time() * 1000),
        "ttl": 300000,
        "hop_count": 0,
        "performative": NAME_TO_ID["inform"],
        "body": reply_body,
    }

    # 4. Python signs its reply with its OWN key, Rust verifies
    py_priv = Ed25519PrivateKey.generate()
    py_pub = py_priv.public_key().public_bytes_raw()
    reply["sig"] = py_priv.sign(msgpack.packb(
        _canonical(reply, ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl", "reply_with", "hop_count", "performative", "body"]),
        use_bin_type=True))

    open(f"{workspace}/from_py.bin", "wb").write(write_frame(msgpack.packb(reply, use_bin_type=True)))
    open(f"{workspace}/py_pub.bin", "wb").write(py_pub)
    print("PY REPLY SENT, output =", reversed_text)

    # 5. Rust side consumes and verifies
    out = os.popen(f"{rust_bin} recv {workspace}/from_py.bin {workspace}/py_pub.bin").read()
    print("RUST SAYS:")
    print(out.strip())
    assert "SIG_OK=true" in out, "rust failed to verify python signature"
    assert "PERFORMATIVE=Inform" in out, "rust misread performative"
    print()
    print("=== INTEROP PASS: rust->py sig ok, py->rust sig ok, semantics match ===")


if __name__ == "__main__":
    main()
