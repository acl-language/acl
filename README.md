# ACL — Agent Communication Language

[![CI](https://github.com/acl-language/acl/actions/workflows/ci.yml/badge.svg)](https://github.com/acl-language/acl/actions/workflows/ci.yml)

A compact, binary, platform-agnostic protocol for communication between
autonomous AI agents. Designed as an open standard: the same signed envelope
runs over stdio between two local processes and over HTTP/SSE across the
network — no server required, no framework lock-in.

**Read the spec: [SPEC.md](SPEC.md)** — 33 sections, normative, with a
compliance checklist and an independent-implementation interop method.

## Why

Existing protocols leave real gaps:

- **MCP** connects an agent to *tools*, not to other agents
- **A2A** assumes agents are network services with stable HTTP endpoints —
  two local processes on one machine are left out
- None carry **speech-act semantics** (performatives), only RPC

ACL fills the local gap and adds FIPA-style performatives (`request`,
`inform`, `query`, `propose`, `retract`, `subscribe`, `input_required`,
`failure`, …) over a compact binary wire format, with Ed25519-signed
envelopes and self-verifying capability cards. Bridges map ACL onto A2A
task states and MCP tools, so an ACL agent can reach the existing
ecosystem without knowing it exists.

## Architecture (spec §2)

```
L6  application profiles
L5  workflow / session state machine
L4  capability descriptors (AgentCard) + discovery (.well-known, did:web)
L3  message semantics (performatives, FP/RE as message data)
L2  envelope (ids, routing, security)
L1  wire format (MessagePack)
L0  transport (stdio/pipes · HTTP · SSE · WebSocket · store-and-forward relay)
```

Above the core: **IDL compiler** (`aclc`: check/lint/fmt/gen rust|py|ts|schema),
**stdlib vocabularies** (fs/task/time/net/proc/http), **dialogue rules** (§16),
**formal semantics** (FP/RE, §17 + §32.7), **runtime schema validation** (§19),
**declarative workflows** (§20), **contract-net negotiation** (§21),
**DID resolution** (§22), **content language with typed values, bindings,
quantification, metanível, semantic time and vocabulary deprecation**
(§6.2, §27-§31), the **expressiveness layer** (§32: belief revision,
subscriptions, conditional commitments, preference policies, error
taxonomy, units of measure) — and **protocol programs**: `.aclp` files
where the conversation itself is the program, compiled and executed with
the dialogue matrix enforced at build time (§33), plus a **conformance
suite** (§23, 10 checks / 3 levels).

## What's here

| component | what it proves |
|-----------|----------------|
| [`acl-core`](src/) (Rust) | codec, signing, AgentCard, framing, bridges, CL (typed), dialogue, semantics, schema, cnp, did, batch, relay, rdf, protocol programs + runtime + projection — 88/88 tests |
| [`acl-peer`](src/bin/peer.rs) | complete agent over stdio: §8 workflow + **speaks the content language** (§6.2) |
| [`acl-httpd`](src/bin/httpd.rs) | HTTP + SSE transport; per-event signed stream |
| [`acl-wsd`](src/bin/wsd.rs) | WebSocket transport (RFC 6455) |
| [`acl-relay`](src/bin/relay.rs) | store-and-forward mailbox relay for intermittent agents (§24) |
| [`acl-mcp-bridge`](src/bin/mcp_bridge.rs) | ACL ↔ real MCP server, end-to-end |
| [`acl-a2a-bridge`](src/bin/a2a_bridge.rs) | ACL ↔ real A2A server (HTTP), end-to-end |
| [`aclc`](src/bin/aclc.rs) | the language compiler: check/lint/fmt/gen (rust, py, ts, schema), `protocol` (.aclp → compile), `endpoints` (global → per-role projection) |
| [`src/protocol.rs`](src/protocol.rs) | **`.aclp` protocol programs** — speech acts as programming primitives; §16 dialogue matrix enforced at COMPILE time |
| [`src/runtime.rs`](src/runtime.rs) | **the runtime**: executes programs as live agents with an auditable transcript; multiparty projection to per-role endpoints |
| [`acl-conformance`](src/bin/acl_conformance.rs) | certification suite: 10 checks, 3 levels |
| [`acl-discover`](src/bin/acl_discover.rs) | generates self-signed AgentCard + did:web DID document |
| [`acl-py`](acl-py/) | native Python binding (PyO3, abi3) |
| [`acl-ts`](acl-ts/) | native TypeScript/Node binding (napi-rs) |
| [`py_codec.py`](tests/interop/py_codec.py) | independent codec written **from the spec only** |
| [playground](https://acl-language.github.io/acl/playground/) | watch a real signed conversation decoded live in the browser |

## Interop proof

The key result: an independent implementation built *only* from SPEC.md
exchanges signed messages with the reference implementation, byte-exact, in
both directions (spec §11.2). Interop testing caught and fixed two real spec
ambiguities — this is the method, not an accident.

## Quick start

```bash
cargo test                              # 88/88 unit tests
cargo build --bin peer
# live conversation over pipes (needs: msgpack, cryptography, node):
python3 tests/interop/stdio_demo.py /tmp/acl tests/debug/peer
# compile a protocol program — the conversation IS the program:
cat > /tmp/supply.aclp <<'EOF'
protocol supply v1;
roles buyer, seller;
on request("quote") from buyer -> send propose to buyer;
on accept from buyer -> begin;
on breach -> send failure to buyer;
end when fulfilled;
EOF
aclc protocol /tmp/supply.aclp      # illegal replies do not compile
aclc endpoints /tmp/supply.aclp     # per-role projections
```

Python:

```python
import acl_py
key = acl_py.AclKey()
env = acl_py.AclEnvelope("local:a:b", "did:x:y", "request",
                         {"t": "Task", "v": {"description": "t", "input": {}}})
env.sign(key)
```

## For AI agents

If you are an AI agent reading this repository: **you can speak ACL.**
This section is written for you. Discovery path, in order:

**1. Load the curated index** — [`llms.txt`](llms.txt) contains
"speak-ACL-in-60-seconds": envelope layout, all 17 performatives, the
content language, and dialogue rules in one page.

**2. Check who you're talking to** — fetch the capability card:

```
https://acl-language.github.io/acl/.well-known/agent-card.json
```

Self-signed (§7): the card embeds its own Ed25519 public key and
signature — verify it before trusting the advertised capabilities (8
capabilities, including `acl.speak`, `acl.schema.validate`,
`acl.relay.store_forward`, `acl.cnp.negotiate`).

**3. Resolve identity** — `did:web:acl-language.github.io:acl` →
[`.well-known/did.json`](https://acl-language.github.io/acl/.well-known/did.json)
(§22). Your own identity is one Ed25519 keypair encoded as
`did:key:z<base58(0xed01 || pubkey)>` — see §22; no registration needed.

**4. Get typed vocabularies** — the compiled schemas describe every
predicate/action with parameter types and deprecation status:

```bash
aclc gen schema stdlib/fs.acl    # → JSON schema view
```

(stdlib: `fs`, `task`, `time`, `net`, `proc`, `http`).

**5. Send your first signed message** — Python:

```python
import acl_py
key = acl_py.AclKey()
env = acl_py.AclEnvelope("local:me:1", "local:peer:1", "inform",
    {"t": "Cl", "v": [{"fact": {"predicate": "greeting", "args": {"lang": "en"},
                                "truth": True}}]})
env.sign(key)
env.encode()          # → msgpack bytes, ready for any transport
```

**6. Mind the dialogue rules (§16)** — a `query` must be answered by a
`fact`; a `propose` is answered by `accept` or `reject`; `bye` closes the
session. Sending an illegal reply is a conformance failure.

**7. Certify your implementation** — the conformance suite (§23) has 10
checks in 3 levels. Level 1 (encode/decode/sign/verify) is achievable
from SPEC.md alone in an afternoon; `py_codec.py` is a working example of
exactly that.

**Bonus — skip writing the agent entirely**: protocol programs (§33) let
you DEFINE the conversation instead of coding handlers. Write an `.aclp`
file, compile it with `aclc protocol`, and the dialogue matrix is
enforced at build time — an agent that answers `query` with `request`
does not compile. `aclc endpoints` projects the global protocol onto
each role's endpoint; the runtime executes programs deterministically
with an auditable transcript.

Everything else — full spec, tutorial, benchmarks, playground — is linked
from [`llms.txt`](llms.txt). Deployed artifacts live on the
[docs site](https://acl-language.github.io/acl/) alongside the rustdoc.

Everything else — full spec, tutorial, benchmarks, playground — is linked
from [`llms.txt`](llms.txt). Deployed artifacts live on the
[docs site](https://acl-language.github.io/acl/) alongside the rustdoc.

## Status

v0.13-draft, Apache-2.0 (SPEC-VERSION 1 on the wire — all changes additive).

Validated end-to-end: stdio/pipes, HTTP sync, SSE streaming (per-event
signatures), WebSocket, store-and-forward relay, Python + TypeScript
bindings, sealed envelopes (X25519 + ChaCha20-Poly1305), idempotency,
local registry, MCP bridge (against a real MCP server), A2A bridge
(against a real A2A server over HTTP), 3-agent delegate-chain demo,
content language (typed facts/actions/queries/commitments with
composition, epistemic modality, provenance and semantic time) spoken by
the reference agent, runtime schema validation with deprecation warnings,
declarative workflows, contract-net negotiation, DID resolution (did:key
offline, did:web), batch signature verification, agent-discovery
artifacts (self-signed AgentCard + did:web), conformance suite (level 3).

### It is a programming language whose primitives are speech acts

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
(FP/RE, §17) — now also **as message data** (§30: `rational_effect`,
`feasibility_precondition`). A **standard library** (`stdlib/`: fs, task,
time, net, proc, http) ships compiled in CI. Expressions compose
(`and/or/not/ref`), bind (`bind`), quantify (`for_all`/`exists`), reason
about other agents (`believes`), and expire (`valid_until_ms`,
`derived_from`).

Agents can **revise beliefs** (`retract`, `supersedes`), **subscribe to
events** (`subscribe` with declarative matchers), make **conditional
commitments**, state **preference policies** (`prefer`), report **typed
failures** (`ErrorKind` taxonomy), and use **units of measure**
(`{"$measured": {"value": 5000, "unit": "ms"}}`).

And with `.aclp` **protocol programs** (§33), the conversation itself is
source code:

```acl
protocol supply v1;
roles buyer, seller;

on request("quote") from buyer -> send propose to buyer;
on accept from buyer -> begin;        // commitment proposed → active
on inform("paid") from buyer -> fulfil;
on breach -> send failure to buyer;

end when fulfilled;
```

- **Compile-time dialogue enforcement**: every `send` is checked against
  the §16 matrix — `on query -> send request` does not compile
- **The runtime executes the program**: events in, actions out, every
  received event in an auditable transcript (handled or not),
  deterministic and replayable
- **Multiparty projection**: `aclc endpoints` compiles the global
  protocol into per-role endpoints — each agent sees only its side;
  two projected runtimes reproduce the global conversation exactly

Also here: [docs/architecture.html](docs/architecture.html) (visual stack map),
[BENCH.md](BENCH.md) (codec benchmarks),
[docs/TUTORIAL.md](docs/TUTORIAL.md) (10-minute guide),
[CHANGELOG.md](CHANGELOG.md) (v0.1.0 → v0.13.0),
[governance/](governance/) (RFC process).

Not yet: sealed-envelope key rotation, public governance body (RFCs
exist; stewards pending), n-role (>2) projection, time-driven handlers
(`every 60s`) in the runtime.

## License

Apache-2.0 (spec and code).
