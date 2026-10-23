# ACL — a programming language for AI agents

[![CI](https://github.com/acl-language/acl/actions/workflows/ci.yml/badge.svg)](https://github.com/acl-language/acl/actions/workflows/ci.yml)

ACL is a programming language whose primitives are **speech acts**. You
write a *protocol program* — a `.aclp` file that declares roles, session
state and handlers keyed on speech acts — and the compiler turns it into
agents that speak the protocol, with the conversation rules enforced at
build time. An agent compiled from a protocol program cannot deviate from
the protocol it declares.

Everything below is **implemented and tested**: 130/130 unit tests, plus
an independent implementation written from the spec alone proving
byte-exact wire interoperability.

## A protocol program

```acl
protocol supply v1;
roles buyer, seller;
state quoted: bool;

on request("quote") from buyer -> send propose to buyer;
on accept from buyer when quoted -> begin;      // commitment proposed → active
on inform("paid") from buyer -> fulfil;         // active → fulfilled
on breach -> send failure to buyer;             // compensation
every 60s while active as seller -> send progress to buyer;

end when fulfilled;

scenario "happy path" {
  buyer sends request("q") to seller;
  expect seller sends propose;
  expect buyer sends accept;
  expect commitment seller active;
}
```

Compile it, project it per role, generate runnable agents, run the
embedded tests:

```bash
aclc protocol supply.aclp          # illegal replies do not compile
aclc endpoints supply.aclp         # per-role projections
aclc gen agent py supply.aclp out/ # runnable Python agent per role
aclc scenarios supply.aclp         # runs the embedded scenarios
```

## What the language has

**Speech acts (§5, §32)** — 17 performatives with a dialogue legality
matrix enforced at compile time: `request`, `inform`, `query`, `propose`,
`accept`, `reject`, `cancel`, `progress`, `input_required`, `failure`,
`delegate`, `error`, `hello`, `bye`, `retract`, `subscribe`,
`unsubscribe`.

**Commitments (§6.2)** — verifiable promises with a lifecycle
(proposed → active → fulfilled / breached / cancelled), conditional
activation, deadline renewal, compensation on breach. Mis-sequenced
transitions are logged, never fatal.

**Content language (§6.2, §27-§32)** — typed values (`Str`, `Int`,
`Float`, `Bool`, `Var`, `List`, `Map`, `Bytes`, `Measured`), fact
composition (`and`/`or`/`not`), quantification (`for_all`/`exists`),
belief reasoning (`believes`), lexical bindings, semantic time
(`valid_until_ms`, `derived_from`), belief revision (`retract`,
`supersedes`), preference policies (`prefer`), a structured error
taxonomy, and units of measure.

**Session state and computation (§34-§35)** — typed states
(`bool|int|str|list|map`), an expression language with 18 pure builtins,
`set`/`push`/`put`, handler guards (`when quoted`), `for` fan-out over
collections, message templates with `fmt()`, time-driven handlers
(`every 60s while active as seller` — the runtime stays clock-free: the
host injects `tick()`), declarative retry
(`send X retry 3 backoff 2s exp`), abort with compensation
(`abort when <guard>`, latched).

**Composition and modules (§39)** — expression guards over typed
pattern captures (`when $p > 100`), trigger alternation
(`on (accept | reject)`), reusable handler fragments with visibility
(`handler_set` + `include handler_set ns.name;`, `export` marks what is
importable), and module imports (`import "common.aclp" as m;`) with
namespace collision detection.

**Static typing** — `set` assignments are type-checked at compile time
(int widens to float; captures and `arg.*` stay runtime-checked), and
record field access/literals are validated against declared types.

**Errors as flow** — internal failures (bad `set`/`push`/`put`, retry
exhaustion) route to an `on error` handler with `$reason`, guarded
against error loops.

**Contract-net** — `broadcast propose to bidders;` fans out one send per
role in a list state; `prefer(bids, "price")` picks the lowest bid.

**Rich templates** — `fmt("price ${p} total ${total + 2}")` interpolates
full expressions inline; registered templates late-bind `${expr}` at the
call site and keep positional `{0}` placeholders.

**N-role protocols (§42)** — protocols with any role count: `check`
forces explicit attribution (`from` on triggers, `to` on sends) and
`on X from R as <role>` pins the executor to one role.

**Declarative collection (§43)** — `await 3 propose into bids within 5s
as ready;` collects matching payloads into a list state and completes
(or times out) a bool flag — contract-net without hand-written handlers.

**Local error flow (§44)** — `try { ... } catch { ... };` and the
`handle error` sugar: a failing action cuts the body and runs the catch
locally instead of the session-level `on error`.

**Typed wire fields (§45)** — payloads carry native int/float/bool
values end-to-end; captures resolve without string heuristics.

**Compile-time guarantees** — dialogue matrix (an illegal reply is a
compile error), linearity (every send must have a receiver — orphan
sends are compile errors), liveness (handlers that cannot reach a
terminal action warn as possible deadlocks), full static validation of
guards, state type lattice (`list<T>`, `map<K,V>`, records), typed
assignments, record fields, and function recursion.

**Toolchain** — `aclc fmt` canonical (idempotent) formatting of any
program; scenario assertions `expect state/commitment/log`.

### Inheritance and modules (§41, §39.8)
`protocol supply_v2 v1 extends supply_v1;` inherits everything and may
override handlers by trigger+guard signature; `export type X;` /
`export fn f;` make vocabulary importable alongside handler sets.

## The toolchain

`aclc` compiles the language:

| command | what it does |
|---|---|
| `check` / `lint` / `fmt` / `names` | parse, style, canonical print on `.acl` vocabularies |
| `gen rust\|py\|ts\|schema` | typed builders and JSON schema views |
| `protocol <file.aclp>` | compile a protocol program |
| `endpoints <file.aclp>` | project the global protocol onto per-role endpoints |
| `gen agent py <file.aclp> <dir>` | emit a **runnable Python agent skeleton per role** |
| `scenarios <file.aclp>` | run the embedded scenario tests |
| `trace <file.aclp> <journal.jsonl>` | step-through replay debugger |
| `publish` / `protocols` / `fetch` | protocol registry with SHA-256 integrity |

## The runtime

Deterministic — a pure function of (program, delivered events):

- every received event enters an **auditable transcript**, handled or not
- **event sourcing**: replaying a journal reconstructs exact state
  (soak-verified: 200 concurrent sessions × 25 events, exact-state replay)
- **multi-session host** with declared capacity and isolated session state
- **capability enforcement, default-on**: a program with `use`
  declarations cannot open sessions until the environment grants them

## Security

- Ed25519-signed envelopes; batch signature verification
- Sealed envelopes: X25519 + ChaCha20-Poly1305 with **HKDF-SHA256 key
  derivation** (raw shared secrets are never symmetric keys)
- Per-session key derivation; **identity rotation** signed by the old key
  (provable continuity)
- Bearer-token transport auth (`acl-httpd --token`, `acl-wsd --token`)

## Interoperability

- Bridges to **A2A** (bidirectional task mapping) and **MCP**
  (tools/call with JSON-schema-lite argument validation)
- Native bindings: **Python** (PyO3, abi3) and **TypeScript** (napi-rs)
- Discovery: self-signed AgentCard (`.well-known/agent-card.json`),
  `did:web` DID document, `llms.txt` for AI agents
- Transports: stdio/pipes, HTTP + SSE, WebSocket, store-and-forward relay
- Registries: agents (file-backed) and protocols (checksummed)

## Tooling

- **Language server** (`acl-lsp`): diagnostics for parse, dialogue,
  linearity, liveness and guard errors; completion for keywords,
  performatives and program symbols
- **Conformance suite**: 10 checks in 3 levels
- **Playground**: watch a real signed conversation decoded live —
  https://acl-language.github.io/acl/playground/
- Architecture map: [docs/architecture.html](docs/architecture.html)

## Standard library

- Vocabularies (`stdlib/*.acl`): `fs`, `task`, `time`, `net`, `proc`,
  `http`, `agent_ops` (telemetry, tool calls, delegation, heartbeats, SLA)
- Protocol programs (`stdlib/protocols/`): `pilot_ops.aclp` —
  orchestrator delegates a tool call with an SLA commitment
- Fragments: `deadline_escalation`, `double_confirmation`

## Getting started

```bash
cargo install --path .           # installs aclc + the binaries
cargo test                       # 130/130
python3 tests/interop/stdio_demo.py /tmp/acl target/debug/peer
```

Python:

```python
import acl_py
key = acl_py.AclKey()
env = acl_py.AclEnvelope("local:me:1", "local:peer:1", "inform",
    {"t": "Cl", "v": [{"fact": {"predicate": "greeting",
                                "args": {"lang": "en"}, "truth": True}}]})
env.sign(key)
env.encode()   # → msgpack bytes, ready for any transport
```

## Documents

- **Spec**: [SPEC.md](SPEC.md) — 38 sections, normative
- **Grammar**: [GRAMMAR.md](GRAMMAR.md) — formal EBNF definition of the language
- **For AI agents**: [llms.txt](llms.txt)
- **Tutorial**: [docs/TUTORIAL.md](docs/TUTORIAL.md) — two agents talking in 10 minutes
- **Benchmarks**: [BENCH.md](BENCH.md) — codec + ACL-vs-natural-language comparison
- **Changelog**: [CHANGELOG.md](CHANGELOG.md)
- **Governance**: [governance/](governance/) — RFC process
- **Interop method**: spec §11.2 — an independent implementation built
  *only* from SPEC.md exchanges signed messages with the reference
  implementation, byte-exact, in both directions

## License

Apache-2.0 (spec and code).
