#!/usr/bin/env python3
"""stdio transport demo (SPEC.md §9 + §8 workflow).

A live Python client speaks ACL over real pipes with the Rust acl-peer
process: request → result, request → input_required → inform → result,
and a signed agent.card query. No files as message channel — actual pipes.

Reuses the independent codec from py_codec.py (written from SPEC.md only).
"""

import os, sys, subprocess, time, secrets
import msgpack
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame, write_frame

FIELD_ORDER = ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl",
               "reply_with", "hop_count", "performative", "body"]


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def canonical(env):
    out = {}
    for f in FIELD_ORDER:
        v = key_of(env, f)
        if v is None:
            continue
        if f == "hop_count" and v == 0:
            continue
        out[f] = v
    return out


class Peer:
    def __init__(self, rust_bin, workdir, name):
        self.pub_file = os.path.join(workdir, f"{name}.pub")
        if os.path.exists(self.pub_file):
            os.remove(self.pub_file)
        self.p = subprocess.Popen(
            [rust_bin, "--name", name, "--pub-out", self.pub_file],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL)
        for _ in range(100):
            if os.path.exists(self.pub_file):
                break
            time.sleep(0.05)
        self.pub = open(self.pub_file, "rb").read()

    def send(self, env):
        self.p.stdin.write(write_frame(msgpack.packb(env, use_bin_type=True)))
        self.p.stdin.flush()

    def recv(self):
        payload, rest = read_frame(self.p.stdout.read1(1 << 20))
        return msgpack.unpackb(payload, raw=False)


def make_env(from_aid, to_aid, perf_name, body, in_reply_to=None):
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": from_aid,
        "to": to_aid,
        "ts": int(time.time() * 1000),
        "ttl": 300000,
        "performative": NAME_TO_ID[perf_name],
        "body": body,
    }
    if in_reply_to:
        env["in_reply_to"] = in_reply_to
    return env


def main():
    workdir = sys.argv[1]
    rust_bin = sys.argv[2]
    peer = Peer(rust_bin, workdir, "stdio-demo")
    me = "local:pyclient:demo"
    print("client:", me, "| peer pubkey:", peer.pub[:8].hex(), "…")

    # --- 1. request → result (happy path)
    env = make_env(me, "local:peer:stdio-demo", "request", {
        "t": "Task",
        "v": {"description": "reverse", "input": {"text": "stdio-pipes"},
              "deadline_ms": 30000},
    })
    peer.send(env)
    reply = peer.recv()
    assert verify_sig(reply, peer.pub), "peer signature invalid"
    assert PERFORMATIVE[key_of(reply, "performative")] == "inform"
    out = key_of(reply, "body")["v"]["output"]["reversed"]
    assert out == "sepip-oidts", out
    print(f"[1] request -> inform/Result: reversed = {out!r}  (sig OK)")

    # --- 2. request missing input -> input_required -> inform -> result (§8)
    env = make_env(me, "local:peer:stdio-demo", "request", {
        "t": "Task",
        "v": {"description": "reverse-no-input", "input": {}},
    })
    first_id = env["msg_id"]
    peer.send(env)
    reply = peer.recv()
    assert verify_sig(reply, peer.pub)
    assert PERFORMATIVE[key_of(reply, "performative")] == "input_required"
    need = key_of(reply, "body")["v"]
    print(f"[2] request -> input_required: needs {need['what']!r}")
    assert txt(key_of(reply, "in_reply_to")) == first_id

    ans = make_env(me, "local:peer:stdio-demo", "inform",
                   {"t": "Text", "v": {"text": "here is the text: acl"}},
                   in_reply_to=txt(key_of(reply, "msg_id")))
    peer.send(ans)
    final = peer.recv()
    assert verify_sig(final, peer.pub)
    assert PERFORMATIVE[key_of(final, "performative")] == "inform"
    out = key_of(final, "body")["v"]["output"]
    print(f"[2] resumed -> inform/Result: {out}")
    assert out["completed"] == "reverse-no-input"

    # --- 3. query agent.card -> Hello with signed AgentCard
    env = make_env(me, "local:peer:stdio-demo", "query",
                   {"t": "Query", "v": {"subject": "agent.card"}})
    peer.send(env)
    reply = peer.recv()
    assert verify_sig(reply, peer.pub)
    card = key_of(reply, "body")["v"]["agent_card"]
    card_pub = card["public_key"]
    card_canon = {k: v for k, v in card.items() if k != "sig"}
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    Ed25519PublicKey.from_public_bytes(card_pub).verify(
        card["sig"], msgpack.packb(card_canon, use_bin_type=True))
    print(f"[3] agent.card verified: {card['name']} v{card['version']}, "
          f"capabilities = {[c['name'] for c in card['capabilities']]}")

    # --- 4. bye
    peer.send(make_env(me, "local:peer:stdio-demo", "bye",
                       {"t": "Ref", "v": {"target_msg_id": first_id}}))
    peer.p.wait(timeout=5)
    print("[4] bye -> peer exited rc =", peer.p.returncode)

    print()
    print("=== STDIO TRANSPORT PASS: 4 exchanges live over pipes, all sigs verified ===")


if __name__ == "__main__":
    main()
