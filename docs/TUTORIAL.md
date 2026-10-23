# ACL in 10 minutes

This tutorial takes you from zero to two signed agents exchanging tasks.

## 0. What you'll build

Two agents on the same machine — no server, no ports — exchanging
Ed25519-signed ACL envelopes over pipes, including a task that pauses for
`input_required` and resumes.

```text
you (Python client)  ──pipes──►  acl-peer (Rust agent)
```

## 1. Build (2 min)

```bash
git clone https://github.com/acl-language/acl && cd acl
cargo test        # 18+ unit tests
cargo build --bin peer
```

## 2. The mental model (2 min)

Every message is one **envelope**:

```text
acl=1, msg_id, in_reply_to?, from, to, ts, ttl,
performative, body, sig
```

- `performative` is the *intent*: `request`, `inform`, `progress`,
  `input_required`, `failure`, … (14 total, u8 on the wire)
- `body` is a tagged variant: `{"t": "Task", "v": {...}}`
- `sig` is Ed25519 over the canonical envelope minus `sig` (spec §10)

An agent is just: **read frame → decode → act → reply envelope → sign → frame**.

## 3. Run the live demo (3 min)

```bash
mkdir -p /tmp/acl-demo
python3 tests/interop/stdio_demo.py /tmp/acl-demo target/debug/peer
```

You'll see: `request → result`, then `request → input_required →
inform → result`, then a signed AgentCard query. All over raw pipes.

## 4. Write your own client (3 min)

Python, using the binding:

```python
import acl_py
key = acl_py.AclKey()
env = acl_py.AclEnvelope("local:me:1", "local:peer:1", "request",
    {"t": "Task", "v": {"description": "reverse", "input": {"text": "hi"}}})
env.sign(key)
# write_frame(env.encode()) to the agent's stdin, read the reply frame,
# AclEnvelope.decode(...).verify(agent_pub)
```

(Or without the binding: `tests/interop/py_codec.py` is a readable
implementation of the whole codec in ~150 lines — the spec is small.)

## 5. Where to go next

| goal | read / run |
|------|------------|
| the protocol | `SPEC.md` (14 sections) |
| multi-agent + delegate chains | `python3 tests/pilots/demo.py` |
| talk to MCP servers | `python3 tests/mcp/bridge_demo.py` (needs node) |
| HTTP/SSE (network) | `cargo build --bin httpd` + `tests/interop/http_demo.py` |
| WebSocket | `cargo build --bin wsd` + `tests/interop/ws_demo.py` |
| everything at once | `scripts/quickstart.sh` |

## Troubleshooting

- **`verify` returns false across implementations** → you almost certainly
  disagree on canonical encoding. Spec §10: omit absent Option fields AND
  zero `hop_count` before signing.
- **msgpack decode errors** → performatives are u8, not strings (spec §5).
- **Agent hangs** → you probably didn't frame (4-byte BE length) or the
  peer waits for EOF.
