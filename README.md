# ACL — Agent Communication Language

[![CI](https://github.com/acl-language/acl/actions/workflows/ci.yml/badge.svg)](https://github.com/acl-language/acl/actions/workflows/ci.yml)

A compact, binary, platform-agnostic protocol for communication between
autonomous AI agents. Designed as an open standard: the same signed envelope
runs over stdio between two local processes and over HTTP/SSE across the
network — no server required, no framework lock-in.

**Read the spec: [SPEC.md](SPEC.md)** — 14 sections, normative, with a
compliance checklist and an independent-implementation interop method.

## Why

Existing protocols leave real gaps:

- **MCP** connects an agent to *tools*, not to other agents
- **A2A** assumes agents are network services with stable HTTP endpoints —
  two local processes on one machine are left out
- None carry **speech-act semantics** (performatives), only RPC

ACL fills the local gap and adds FIPA-style performatives (`request`,
`inform`, `query`, `propose`, `input_required`, `failure`, …) over a compact
binary wire format, with Ed25519-signed envelopes and self-verifying
capability cards. Bridges map ACL onto A2A task states and MCP tools, so an
ACL agent can reach the existing ecosystem without knowing it exists.

## Architecture (spec §2)

```
L6  application profiles
L5  workflow / session state machine
L4  capability descriptors (AgentCard)
L3  message semantics (performatives)
L2  envelope (ids, routing, security)
L1  wire format (MessagePack)
L0  transport (stdio/pipes · HTTP · SSE · [WebSocket])
```

## What's here

| component | what it proves |
|-----------|----------------|
| [`acl-core`](src/) (Rust) | codec, signing, AgentCard, framing, bridges — 11/11 tests |
| [`acl-peer`](src/bin/peer.rs) | complete agent over stdio; `input_required` workflow live |
| [`acl-httpd`](src/bin/httpd.rs) | HTTP + SSE transport; per-event signed stream |
| [`acl-mcp-bridge`](src/bin/mcp_bridge.rs) | ACL ↔ real MCP server, end-to-end |
| [`acl-py`](acl-py/) | native Python binding (PyO3, abi3) |
| [`py_codec.py`](tests/interop/py_codec.py) | independent codec written **from the spec only** |

## Interop proof

The key result: an independent implementation built *only* from SPEC.md
exchanges signed messages with the reference implementation, byte-exact, in
both directions (spec §11.2). Interop testing caught and fixed two real spec
ambiguities — this is the method, not an accident.

## Quick start

```bash
cargo test                              # 11/11 unit tests
cargo build --bin peer
# live conversation over pipes (needs: msgpack, cryptography, node):
python3 tests/interop/stdio_demo.py /tmp/acl tests/debug/peer
```

Python:

```python
import acl_py
key = acl_py.AclKey()
env = acl_py.AclEnvelope("local:a:b", "did:x:y", "request",
                         {"t": "Task", "v": {"description": "t", "input": {}}})
env.sign(key)
```

## Status

v0.2-draft, Apache-2.0 (SPEC-VERSION 1 on the wire — all changes additive).

Validated end-to-end: stdio/pipes, HTTP sync, SSE streaming (per-event
signatures), WebSocket, Python + TypeScript bindings, sealed envelopes
(X25519 + ChaCha20-Poly1305), idempotency, local registry, MCP bridge
(against a real MCP server), A2A bridge (against a real A2A server over
HTTP), 3-agent delegate-chain demo, content language (facts/actions/
queries/commitments) spoken by the reference agent.

### It is a language, not just a protocol

Vocabularies are **declared, typed and compiled**. Write an `.acl` IDL file:

```acl
namespace acl.std.fs v1;
predicate file_exists(path: Path) -> bool;
action file_read(path: Path) -> string;
```

…then `aclc check|lint|fmt|gen rust|py|ts` produces typed builders for
facts, actions, queries and commitments (with provenance, confidence,
epistemic modality and expiry), enforces the per-performative grammar
matrix, the dialogue reply policy (§16) and carries formal semantics
(FP/RE, §17). A **standard library** (`stdlib/`: fs, task, time, net)
ships compiled in CI. Expressions compose (`and/or/not/ref`).

Also here: [BENCH.md](BENCH.md) (codec benchmarks),
[docs/TUTORIAL.md](docs/TUTORIAL.md) (10-minute guide),
[governance/](governance/) (RFC process).

Not yet: sealed-envelope key rotation, batch signature verification,
public governance body (RFCs exist; stewards pending).

## License

Apache-2.0 (spec and code).
