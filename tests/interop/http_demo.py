#!/usr/bin/env python3
"""HTTP + SSE transport demo (SPEC.md §9).

Python client talks to the Rust acl-httpd over real TCP:
  1. POST /        — framed signed envelope, sync reply, sig verified
  2. GET /events   — SSE stream: progress envelopes then final Result
  3. GET /agent-card — signed AgentCard, verified
"""

import os, sys, time, secrets, socket
import msgpack
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame

import base64

FIELD_ORDER = ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl",
               "reply_with", "hop_count", "performative", "body"]


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def http_request(port, method, path, body=None):
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    head = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
    if body is not None:
        head += f"Content-Length: {len(body)}\r\n"
    head += "\r\n"
    s.sendall(head.encode() + (body or b""))
    resp = b""
    while True:
        try:
            chunk = s.recv(65536)
        except socket.timeout:
            break
        if not chunk:
            break
        resp += chunk
    s.close()
    header, _, payload = resp.partition(b"\r\n\r\n")
    return header.decode("latin1"), payload


def main():
    port = int(sys.argv[1])
    pub = open("/tmp/acl-httpd.pub", "rb").read()
    me = "local:pyclient:http"
    peer = "http://127.0.0.1/agent/acl-httpd"
    print("client:", me, "| server pubkey:", pub[:8].hex(), "…")

    # --- 1. POST / : sync request → result
    env = {
        "acl": 1,
        "msg_id": secrets.token_bytes(16).hex(),
        "from": me, "to": peer,
        "ts": int(time.time() * 1000), "ttl": 300000,
        "performative": NAME_TO_ID["request"],
        "body": {"t": "Task", "v": {"description": "reverse",
                                    "input": {"text": "http-transport"}}},
    }
    body = len(msgpack.packb(env, use_bin_type=True)).to_bytes(4, "big") + msgpack.packb(env, use_bin_type=True)
    header, payload = http_request(port, "POST", "/", body)
    assert " 200 " in header.split("\r\n")[0], header.split("\r\n")[0]
    reply_env = msgpack.unpackb(read_frame(payload)[0], raw=False)
    assert verify_sig(reply_env, pub), "server signature invalid"
    assert PERFORMATIVE[key_of(reply_env, "performative")] == "inform"
    assert txt(key_of(reply_env, "in_reply_to")) == env["msg_id"]
    out = key_of(reply_env, "body")["v"]["output"]["reversed"]
    assert out == "tropsnart-ptth", out
    print(f"[1] POST / -> inform/Result: {out!r} (sig OK, in_reply_to OK)")

    # --- 2. GET /events : SSE stream with progress then result
    header, stream = http_request(port, "GET", "/events?text=sse-stream")
    assert "text/event-stream" in header, header
    events = []
    for block in stream.split(b"\n\n"):
        if not block.strip():
            continue
        lines = block.decode().split("\n")
        data = next(l[6:] for l in lines if l.startswith("data: "))
        ev = msgpack.unpackb(base64.b64decode(data), raw=False)
        assert verify_sig(ev, pub), "SSE event signature invalid"
        events.append((PERFORMATIVE[key_of(ev, "performative")], ev))
    kinds = [k for k, _ in events]
    assert kinds == ["progress", "progress", "progress", "inform"], kinds
    for k, ev in events[:-1]:
        p = key_of(ev, "body")["v"]
        print(f"[2] SSE progress: {p['percent']}% — {txt(p['note'])}")
    final = events[-1][1]
    result = key_of(final, "body")["v"]["output"]
    assert result["reversed"] == "maerts-ess", result
    print(f"[2] SSE final Result: {result} (all 4 events signed OK)")

    # --- 3. GET /agent-card
    header, payload = http_request(port, "GET", "/agent-card")
    env = msgpack.unpackb(read_frame(payload)[0], raw=False)
    assert verify_sig(env, pub)
    card = key_of(env, "body")["v"]["agent_card"]
    card_canon = {k: v for k, v in card.items() if k != "sig"}
    Ed25519PublicKey.from_public_bytes(card["public_key"]).verify(
        card["sig"], msgpack.packb(card_canon, use_bin_type=True))
    print(f"[3] agent-card: {card['name']} endpoint={card['endpoint']} "
          f"(card sig OK, envelope sig OK)")

    print()
    print("=== HTTP/SSE TRANSPORT PASS: sync POST + SSE stream + signed card ===")


if __name__ == "__main__":
    main()
