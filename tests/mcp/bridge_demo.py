#!/usr/bin/env python3
"""End-to-end ACL→MCP bridge demo.

Python (ACL) --pipes--> acl-mcp-bridge (Rust) --stdio--> real MCP server (Node).

Exercises:
  1. request/Task  -> MCP tools/call -> inform/Result (text reversed BY THE
     MCP SERVER, not by the bridge)
  2. query/agent.card -> MCP tools/list -> signed ACL AgentCard with the
     MCP tool exposed as ACL capability
"""

import os, subprocess, sys, time, secrets
import msgpack
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "interop"))
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame, write_frame


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def main():
    workdir = sys.argv[1]
    bridge_bin = sys.argv[2]
    server_js = sys.argv[3]
    pub_file = os.path.join(workdir, "bridge.pub")
    if os.path.exists(pub_file):
        os.remove(pub_file)

    p = subprocess.Popen(
        [bridge_bin, "--mcp", "node", server_js, "--pub-out", pub_file],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)

    for _ in range(100):
        if os.path.exists(pub_file):
            break
        time.sleep(0.05)
    pub = open(pub_file, "rb").read()
    print("client -> bridge pubkey:", pub[:8].hex(), "…")

    def exchange(env):
        p.stdin.write(write_frame(msgpack.packb(env, use_bin_type=True)))
        p.stdin.flush()
        payload, _ = read_frame(p.stdout.read1(1 << 20))
        return msgpack.unpackb(payload, raw=False)

    # --- 1. request through the bridge into real MCP tools/call
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": "local:pyclient:mcp", "to": "local:bridge:mcp-bridge",
        "ts": int(time.time() * 1000), "ttl": 300000,
        "performative": NAME_TO_ID["request"],
        "body": {"t": "Task", "v": {"description": "text.reverse",
                                    "input": {"text": "real-mcp-server"}}},
    }
    reply = exchange(env)
    assert verify_sig(reply, pub), "bridge signature invalid"
    assert PERFORMATIVE[key_of(reply, "performative")] == "inform"
    out = key_of(reply, "body")["v"]["output"]
    assert out["reversed"] == "revres-pcm-laer", out
    assert out["via"] == "mcp", out
    print(f"[1] request -> MCP tools/call -> inform/Result: {out}")

    # --- 2. agent.card backed by real MCP tools/list
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": "local:pyclient:mcp", "to": "local:bridge:mcp-bridge",
        "ts": int(time.time() * 1000), "ttl": 300000,
        "performative": NAME_TO_ID["query"],
        "body": {"t": "Query", "v": {"subject": "agent.card"}},
    }
    reply = exchange(env)
    assert verify_sig(reply, pub)
    assert PERFORMATIVE[key_of(reply, "performative")] == "hello"
    card = key_of(reply, "body")["v"]["agent_card"]
    # verify the card's own signature
    card_canon = {k: v for k, v in card.items() if k != "sig"}
    Ed25519PublicKey.from_public_bytes(card["public_key"]).verify(
        card["sig"], msgpack.packb(card_canon, use_bin_type=True))
    caps = [c["name"] for c in card["capabilities"]]
    print(f"[2] agent.card from MCP tools/list: caps={caps} (card sig OK)")
    assert "text.reverse" in caps

    p.stdin.close()
    p.wait(timeout=5)
    print()
    print("=== ACL<->MCP BRIDGE PASS: real MCP server did the work, "
          "ACL semantics + signatures on both sides ===")


if __name__ == "__main__":
    main()
