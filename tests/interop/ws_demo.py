#!/usr/bin/env python3
"""WebSocket transport demo (SPEC.md §9.3): Python client <-> acl-wsd (Rust)."""

import asyncio, sys, time, secrets
import msgpack
import websockets
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, sys.argv[3] if len(sys.argv) > 3 else ".")
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


async def main():
    port = sys.argv[1]
    pub = open("/tmp/acl-wsd.pub", "rb").read()
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": "local:pyclient:ws", "to": "ws://127.0.0.1/agent/acl-wsd",
        "ts": int(time.time() * 1000), "ttl": 300000,
        "performative": NAME_TO_ID["request"],
        "body": {"t": "Task", "v": {"description": "reverse",
                                    "input": {"text": "websocket"}}},
    }
    async with websockets.connect(f"ws://127.0.0.1:{port}") as ws:
        await ws.send(msgpack.packb(env, use_bin_type=True))
        reply = msgpack.unpackb(await ws.recv(), raw=False)
    assert verify_sig(reply, pub), "ws reply signature invalid"
    assert PERFORMATIVE[key_of(reply, "performative")] == "inform"
    out = key_of(reply, "body")["v"]["output"]
    assert out["reversed"] == "tekcosbew", out
    assert out["via"] == "ws"
    print(f"[ws] request -> inform/Result: {out} (sig OK)")
    print("=== WEBSOCKET TRANSPORT PASS ===")


asyncio.run(main())
