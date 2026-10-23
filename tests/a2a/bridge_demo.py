#!/usr/bin/env python3
"""A2A bridge demo: Python (ACL) <-> acl-a2a-bridge (Rust) <-> real A2A server (node, HTTP)."""

import os, subprocess, sys, time, secrets
import msgpack

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "interop"))
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame, write_frame


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def main():
    workdir = sys.argv[1]
    bridge = sys.argv[2]
    pub_file = os.path.join(workdir, "a2a.pub")
    if os.path.exists(pub_file):
        os.remove(pub_file)
    # bridge writes its pub to /tmp/acl-a2a.pub? simpler: derive from signed reply verification failure -> instead we pass pubkey via file written by bridge
    # Bridge doesn't have --pub-out; verify using the pub file we generate from first reply? We need trust. For the demo we fetch it from bridge stderr? Use /tmp/acl-a2a.pub written below.
    p = subprocess.Popen([bridge, "--a2a", "http://127.0.0.1:9400"],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL)

    def exchange(env):
        p.stdin.write(write_frame(msgpack.packb(env, use_bin_type=True)))
        p.stdin.flush()
        payload, _ = read_frame(p.stdout.read1(1 << 20))
        return msgpack.unpackb(payload, raw=False)

    # request through the bridge into the real A2A server
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": "local:pyclient:a2a", "to": "local:bridge:a2a-bridge",
        "ts": int(time.time() * 1000), "ttl": 300000,
        "performative": NAME_TO_ID["request"],
        "body": {"t": "Task", "v": {"description": "text.reverse",
                                    "input": {"text": "real-a2a-server"}}},
    }
    reply = exchange(env)
    assert PERFORMATIVE[key_of(reply, "performative")] == "inform"
    out = key_of(reply, "body")["v"]["output"]
    assert out["reversed"] == "revres-a2a-laer", out
    assert out["via"] == "a2a", out
    print(f"[1] request -> A2A message/send -> inform/Result: {out}")

    # agent.card from the A2A Agent Card over the network
    env = dict(env, msg_id=secrets.token_bytes(16).hex(),
               performative=NAME_TO_ID["query"],
               body={"t": "Query", "v": {"subject": "agent.card"}})
    reply = exchange(env)
    assert PERFORMATIVE[key_of(reply, "performative")] == "hello"
    card = key_of(reply, "body")["v"]["agent_card"]
    caps = [c["name"] for c in card["capabilities"]]
    print(f"[2] agent.card from A2A Agent Card: name={txt(key_of(reply,'body')['v']['agent_card']['name'])} caps={caps}")
    assert "text.reverse" in caps

    p.stdin.close()
    p.wait(timeout=5)
    print()
    print("=== ACL<->A2A BRIDGE PASS: real A2A server did the work over HTTP ===")


if __name__ == "__main__":
    main()
