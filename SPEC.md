# ACL — Agent Communication Language

**Version:** 0.16.0-draft
**Status:** Draft specification
**License:** Apache-2.0 (spec + reference implementation)

ACL is a compact, binary, platform-agnostic protocol for communication between
autonomous AI agents. It is designed to run both between processes on the same
host (stdio/pipes) and across networks (HTTP/SSE/WebSocket), with no runtime
dependency on any particular agent framework, model vendor, or cloud.

## 1. Design Goals

1. **Platform-agnostic** — any agent (Rust, Python, TS, Go, …) can speak ACL; the
   wire format is self-contained.
2. **Binary-first, compact** — default encoding is MessagePack (CBOR defined as
   a profile). A canonical JSON mapping exists for debugging and gateways.
3. **Speech-act semantics** — messages carry *performatives* (intent), not just
   payloads. ACL inherits from FIPA-ACL / KQML, adapted to LLM agents.
4. **Local-first** — the same envelope works over stdio/pipes between two
   processes on one machine, with no server, no port, no HTTP.
5. **Cryptographic identity** — Ed25519-signed envelopes; capability
   descriptors ("Agent Cards") are signed and verifiable.
6. **Streaming & long tasks** — first-class task lifecycle with
   `INPUT_REQUIRED` (agent needs something from the caller), progress events,
   and cancellation.

## 2. Layered Architecture

```
┌─────────────────────────────────────────┐
│  L6  Application profiles (domains)     │
├─────────────────────────────────────────┤
│  L5  Workflow / session state machine   │
├─────────────────────────────────────────┤
│  L4  Capability descriptors (AgentCard) │
├─────────────────────────────────────────┤
│  L3  Message semantics (performatives)  │
├─────────────────────────────────────────┤
│  L2  Envelope (ids, routing, security)  │
├─────────────────────────────────────────┤
│  L1  Wire format (MessagePack/CBOR)     │
├─────────────────────────────────────────┤
│  L0  Transport (stdio/pipes/HTTP/SSE/WS)│
└─────────────────────────────────────────┘
```

## 3. Wire Format (L1)

Default profile: **MessagePack** (spec: msgpack.org, any compliant codec).
Optional profiles: CBOR (recommended for constrained devices), canonical JSON
(debug only, MUST NOT be used for signature verification).

Framing on stream transports (stdio, pipes, TCP): each message is prefixed with
a **4-byte big-endian length header**, followed by exactly that many bytes of
encoded envelope. On HTTP transports the body is the encoded envelope; on
SSE each `data:` event is base64 of one envelope.

## 4. Envelope (L2)

Canonical field order (serde struct order is the wire order; receivers MUST
tolerate absent optional fields):

| field        | type      | req | notes                                            |
|--------------|-----------|-----|--------------------------------------------------|
| `acl`        | u8        | ✓   | protocol version, currently `1`                   |
| `msg_id`     | u16 bytes | ✓   | 128-bit random/ULID, hex-encoded                  |
| `in_reply_to`| u16 bytes |     | `msg_id` of the message being answered            |
| `from`       | AID       | ✓   | sender identity                                   |
| `to`         | AID       | ✓   | recipient identity                                |
| `ts`         | i64       | ✓   | unix milliseconds (UTC)                           |
| `ttl`        | u32       |     | time-to-live in ms (default 300000 = 5 min)       |
| `reply_with` | string    |     | conversation/correlation key                      |
| `hop_count`  | u8        |     | incremented by relays, max 8                      |
| `performative`| enum u8  | ✓   | see §5                                            |
| `body`       | variant   | ✓   | performative-specific body (§6)                   |
| `sig`        | 64 bytes  |     | Ed25519 signature over all preceding bytes        |

### 4.1 AID (Agent ID)

```text
AID = scheme ":" opaque

scheme ∈ { did, local, http }
```

- `did:example:abc123` — W3C DID, key material resolvable
- `local:<pid>:<session_id>` — same-host process agents (e.g. `local:4211:9f2a`)
- `http://agent.example.com/` — network agent endpoint

The AID is a UTF-8 string on the wire; parsing is lenient, but the `scheme`
determines how `sig` is verified (DID → resolve key; local → pinned key
negotiated at handshake; http → key from AgentCard fetch).

## 5. Performatives (L3)

| #  | name          | meaning                                              | request-response |
|----|---------------|------------------------------------------------------|------------------|
| 1  | `request`     | ask the recipient to do something                     | yes              |
| 2  | `inform`      | state a fact / report a result                        | no               |
| 3  | `query`       | ask for information (no state change)                 | yes              |
| 4  | `propose`     | offer a deal / task split / plan                      | yes              |
| 5  | `accept`      | agree to a prior `propose`                            | no               |
| 6  | `reject`      | refuse a prior `propose` (may carry reason)           | no               |
| 7  | `cancel`      | abort a prior `request` (graceful)                    | yes (ack)        |
| 8  | `progress`    | intermediate report on a task in flight               | no               |
| 9  | `input_required` | "I cannot continue without X" (blocks)            | yes              |
| 10 | `failure`     | task could not be completed (terminal)                | no               |
| 11 | `delegate`    | forward a task to a third agent (sub-contract)        | yes              |
| 12 | `error`       | protocol/transport-level error                        | no               |
| 13 | `hello`       | handshake / capability exchange                       | yes              |
| 14 | `bye`         | clean session close                                   | no               |

### 5.1 Performatives carry intent, not capability

An ACL message says what the *sender* is doing (`request`, `inform`, …). What
the *recipient* can do is described by its **AgentCard** (§7). The two are
complementary: a client picks a peer from a capability registry, then speaks
performatives to it.

## 6. Body Variants (L3)

The `body` field is a tagged variant. For each performative, the *expected*
variant is normative:

| performative   | body variant |
|----------------|--------------|
| `request`      | `Task`       |
| `inform`       | `Result` or `Text` |
| `query`        | `Query`      |
| `propose`      | `Proposal`   |
| `accept`       | `Ref`        |
| `reject`       | `Ref` + optional `Text` |
| `cancel`       | `Ref`        |
| `progress`     | `Progress`   |
| `input_required` | `InputNeeded` |
| `failure`      | `Failure`    |
| `delegate`     | `Task` + `AID` chain |
| `error`        | `Error`      |
| `hello`        | `Hello` (contains AgentCard or its hash) |
| `bye`          | `Ref` (optional) |

### 6.1 Variant definitions

```text
Task       = { description, input (arbitrary msgpack), deadline?, priority? }
Result     = { output (arbitrary msgpack), artifacts: [ { name, mime, bytes } ] }
Text       = { text }
Query      = { subject, parameters? }
Proposal   = { terms, cost?, valid_until? }
Ref        = { target_msg_id }
Progress   = { target_msg_id, percent?, note? }
InputNeeded= { target_msg_id, what, schema? (msgpack-encoded JSON-schema-like) }
Failure    = { target_msg_id, code, reason, recoverable: bool }
Error      = { code, message, details? }
Hello      = { agent_card | card_hash, nonce }
```

### 6.2 Content language (CL)

The body variants above are structurally typed; the **content language**
adds the shared *vocabulary* agents use inside them. A `Cl` body carries a
list of expressions, each tagged by `op`:

```text
ClExpr  = Fact | Action | Query | Commitment

Fact    = { op:"fact",  predicate, args (object, canonical order), truth=true|false }
Action  = { op:"action", name, params, result_schema? }
Query   = { op:"query",  predicate, args }
Commitment = { op:"commitment", debtor, creditor, act: Action,
                deadline_ms?, status: proposed|active|fulfilled|breached|cancelled }
```

**Grammar matrix (normative, `cl::well_formed`)** — which ops a performative
may carry; receivers SHOULD reject ill-formed bodies:

| performative | allowed CL ops |
|--------------|----------------|
| `inform`     | `fact` (any truth value) |
| `request`    | `action` |
| `query`      | `query` |
| `propose`    | `commitment{status=proposed}` |
| `accept`     | `commitment{status=active}` |
| `delegate`   | `action` |
| others       | CL not allowed (own body variant) |

**Commitment lifecycle:** `proposed → active → fulfilled | breached |
cancelled`; `proposed → cancelled`. All other transitions are invalid.
`accept` on a proposed commitment activates it; the debtor owes the
creditor the act until a terminal state.

**Canonical serialization:** `Fact.args` MUST be an object serialized in
sorted-key order (BTreeMap in the reference) so identical facts produce
identical bytes — signatures stay stable regardless of how callers built
the argument object.

**Example** — inform carrying a fact:

```json
{"t":"Cl","v":{"expressions":[
  {"op":"fact","predicate":"file_exists","args":{"path":"/tmp/report.pdf"},"truth":true}
]}}
```

**Example** — propose carrying a commitment:

```json
{"t":"Cl","v":{"expressions":[
  {"op":"commitment","debtor":"local:123:kenji","creditor":"local:1:argus",
   "act":{"op":"action","name":"text.reverse","params":{"text":"abc"}},
   "deadline_ms":1700000000000,"status":"proposed"}
]}}
```

### 6.3 Compositional operators

CL expressions compose:

```text
And  = { op:"and", exprs: [ClExpr...] }   // all leaves hold
Or   = { op:"or",  exprs: [ClExpr...] }   // at least one holds
Not  = { op:"not", expr: ClExpr }         // negates; wraps ONLY fact leaves
Ref  = { op:"ref", msg_id }               // binds to an expression produced in another message
```

- Composition nests arbitrarily; the §6.2 grammar matrix applies **down to
  leaves** (e.g. an `and` under `inform` may contain only fact leaves).
- `not` wraps fact leaves only (`not(fact(...))`); negating actions or
  commitments is ill-formed.
- `ref` is grammar-neutral: it binds to an expression produced in the
  referenced message, whose well-formedness was checked there. An empty
  `msg_id` is ill-formed.
- Empty `and`/`or` are ill-formed.

### 6.4 Epistemic modality and provenance

Facts are not absolute. Every `fact` MAY carry:

| field | type | meaning |
|-------|------|---------|
| `modality` | `asserted` (default) / `believed` / `known` | how the sender holds the fact: own assertion, second-hand belief, or direct verification |
| `provenance` | [AID...] | who asserted/forwarded the fact, oldest first |
| `confidence` | float [0,1] | sender's confidence |
| `valid_until_ms` | unix ms | temporal validity; after this the fact MUST NOT be treated as holding |

Receivers SHOULD weigh `believed` facts below `known`/`asserted` ones and
MUST discard facts past `valid_until_ms`. Absent fields keep v0.1/v0.2
backward compatibility (defaults: `asserted`, no provenance, no expiry).

## 7. AgentCard (L4)

A signed capability descriptor, analogous to A2A's Agent Card but binary and
self-verifying. Fields:

```text
AgentCard = {
  name, version, description,
  endpoint: AID,
  capabilities: [ { name, input_schema?, output_schema?, examples? } ],
  auth: { scheme: "ed25519" | "none", key_id? },
  rate_hint?,        # optional: requests/minute
  valid_until?,      # expiry
}
```

The AgentCard is itself an ACL `Hello` body or served over `query` with
`subject = "agent.card"`. It is signed with the agent's Ed25519 key; clients
verify before delegating work.

## 8. Session / Workflow (L5)

A **session** is a sequence of messages sharing a `reply_with` key. The state
machine for a delegated task:

```
(request) ──► progress* ──► result
                │
                ├─► input_required ──► (answer: inform) ──► progress* ──► result
                │
                └─► failure
```

`cancel` may be issued from the client at any point before `result`/`failure`.
`input_required` is terminal for that turn: the client MUST answer with a new
`inform` referencing the blocked `target_msg_id` before work resumes.

### 8.1 Idempotency and ordering (normative)

- A client retransmitting (timeout, reconnect) MUST reuse the same `msg_id`
  and the same body. Receivers MUST deduplicate by `msg_id` within the TTL
  window: a duplicate MUST NOT be re-processed, but SHOULD be answered with
  the cached response when one exists.
- The duplicate check is **semantic**: performative + from/to + body. A
  re-signed retransmission (new `ts`, same semantics) is a clean duplicate;
  the same `msg_id` with a DIFFERENT semantics is a protocol violation and
  MUST be rejected and logged.
- Messages between two peers are processed in arrival order; there is no
  global ordering. Applications that need ordering use `reply_with`
  session keys and `in_reply_to` chains.
- Relays MUST NOT reorder messages of the same session.

Timeouts: a client SHOULD consider a task failed if no message (including
`progress`) is received within `ttl` of the last received message.

## 9. Transports (L0)

| transport  | framing                    | notes                          |
|------------|----------------------------|--------------------------------|
| stdio      | 4-byte length header      | local, zero-config             |
| pipes      | 4-byte length header      | local, between processes       |
| TCP        | 4-byte length header      | local/LAN                      |
| HTTP/2     | body = envelope           | remote, request/response       |
| SSE        | `data: base64(envelope)`  | remote streaming               |
| WebSocket  | binary frame = envelope   | remote duplex                  |

The envelope does not change between transports. A message composed over stdio
can be relayed over HTTP untouched (relay increments `hop_count`).

### 9.1 HTTP profile (normative, as implemented and validated)

- `POST /` — request body is the framed envelope (4-byte length header +
  msgpack), or a bare envelope; response body is the framed reply envelope.
  Sync request→response only.
- `GET /agent-card` — framed `Hello` envelope carrying the signed AgentCard.
- `GET /events?...` — `text/event-stream`. Each event is
  `event: acl\ndata: <base64(msgpack envelope)>\n\n`. Progress and the final
  Result are **separately signed envelopes**; a client MUST verify each.
- Content type for envelope bodies: `application/acl-envelope`.
- HTTP/1.1 is sufficient for compliance; HTTP/2 is optional.

### 9.3 WebSocket profile (normative, as implemented and validated)

- Upgrade: standard RFC 6455 HTTP `GET` with `Sec-WebSocket-Key`.
- Each binary frame carries **one msgpack envelope** (a bare envelope, or
  the §3 length-prefixed frame — receivers MUST accept both).
- Client frames MUST be masked (RFC 6455 requirement); server frames are
  not masked.
- One request/response exchange per frame pair; a close frame ends the
  session cleanly.

### 9.2 Transport conformance matrix (v0.1 reference implementation)

| transport | status |
|-----------|--------|
| stdio / pipes | validated end-to-end (live pipes, Python client ↔ Rust peer) |
| HTTP sync (`POST`) | validated end-to-end |
| SSE streaming | validated end-to-end (signed per-event) |
| WebSocket | validated end-to-end (RFC 6455, binary frames) |

## 10. Security (L2)

- **Identity:** Ed25519 keypair per agent. The public key is distributed via
  AgentCard (network) or handshake (`Hello` with `nonce`, local).
- **Integrity:** `sig` covers the serialized envelope excluding the `sig`
  field itself, in canonical field order. Canonical form omits absent
  optional fields (`in_reply_to`, `reply_with`, `sig`) and omits
  `hop_count` when zero — encoders and verifiers MUST agree on this.
- **Replay protection:** `msg_id` uniqueness within a `ttl` window; receivers
  MUST reject `in_reply_to` values older than `ttl` that they never sent.
- **Confidentiality:** out of scope for v0.1 (transport-level TLS is expected
  on network transports). A `sealed` envelope variant is planned for v0.2.

### 10.1 Sealed envelopes (v0.1, implemented)

For end-to-end confidentiality through relays, an envelope may carry a
`Sealed` body:

```text
Sealed = { sender_pub (32B X25519), salt (12B), ciphertext }
```

- Key agreement: X25519 ECDH between the sender's static secret and the
  recipient's public key. The symmetric key is HKDF-SHA256 extracted
  and expanded from the shared secret (salt = the 12-byte message salt,
  info = `acl-sealed-v1-key`) — raw shared secrets are never used as
  symmetric keys (§10.1 hardening, v0.2).
- Encryption: ChaCha20-Poly1305 with AAD = `"acl-sealed-v1"`, nonce = `salt`.
- `sender_pub` travels in the clear so the recipient can derive the same key.
- The outer envelope keeps `msg_id` and `reply_with` of the inner one (so
  correlation survives), but relays cannot read `to`-scoped payloads beyond
  routing fields.
- Verified properties: roundtrip, wrong key rejected, tampered ciphertext
  rejected (AEAD tag).

## 11. Reference Implementation

`acl-core` (Rust) is the reference implementation of L1–L5:

- MessagePack (named-field) encode/decode of `Envelope` and every `Body`
  variant; version check (`acl == 1`) on decode
- Ed25519 sign/verify for envelopes and AgentCards (canonical §10 rule)
- AgentCard + Capability structures with self-signature
- Streaming frame codec (4-byte big-endian length header), 16 MiB cap
- Bridge adapters (§13): A2A state/task mapping, MCP tools mapping

Binaries:

- `acl-peer` — a complete ACL agent over the stdio transport; implements the
  §8 workflow (`request` → `input_required` → `inform` → `result`), serves
  `agent.card` queries, **answers CL fact queries, executes CL actions and
  rejects ill-formed CL bodies (§6.2)**, handles `bye`.
- `acl-httpd` — HTTP + SSE server (§9.1): `POST /`, `GET /events`,
  `GET /agent-card`; per-event signatures on the SSE stream.
- `acl-wsd` — WebSocket server (§9.3).
- `acl-mcp-bridge` — translates between ACL and a real MCP server
  (JSON-RPC over stdio): `tools/call` ↔ `request`/Task, `tools/list` ↔
  signed AgentCard.
- `acl-a2a-bridge` — translates between ACL and a real A2A server (HTTP):
  `message/send` ↔ `request`/Task, Agent Card ↔ AgentCard.
- `aclc` — the language compiler (§18): `check`/`lint`/`fmt`/`gen
  rust|py|ts|schema`/`names`.
- `acl-conformance` — the certification suite (§23): 10 checks, 3 levels.
- `registry_cli` — local agent registry CLI (§13bis).

## 11.1 Python binding (`acl-py`)

Native extension built with PyO3 (abi3), exposing:

```python
import acl_py
key = acl_py.AclKey()                      # Ed25519 keypair
env = acl_py.AclEnvelope(from_aid, to_aid, "request",
                         {"t": "Task", "v": {"description": ..., "input": {...}}},
                         in_reply_to=None, reply_with=None)
env.sign(key); raw = env.encode()
env2 = acl_py.AclEnvelope.decode(raw); env2.verify(pub)
acl_py.write_frame(raw) / acl_py.read_frame(stream)
acl_py.ACL_VERSION, acl_py.new_msg_id()
```

Bodies are plain Python dicts (`{"t": kind, "v": {...}}`); missing optional
fields decode as `None`. Cross-check requirement: an envelope signed by
`acl-py` MUST be verifiable by any independent implementation of §10
(validated against the spec-only Python codec).

## 11.2 Independent-implementation interop

Interoperability of this spec is proven by an independent codec implemented
**from this document only** (no reference source consulted):

- Decodes every field of a reference-encoded envelope
- Verifies reference signatures using only the §10 canonical rule
- Its signed envelopes are verified by the reference implementation

Any ambiguity discovered this way is a spec bug and MUST be fixed here
(this is how the canonical-encoding rule in §10 and the `u8` performative
discriminant in §5 were pinned down).

## 12. Compliance

An implementation is ACL-0.1 compliant if it can:
1. Encode/decode every performative in §5 with its body variant from §6
   (performatives on the wire are the **u8 discriminant**, never a string).
2. Frame messages on at least one transport from §9 (§9.1 profile for HTTP).
3. Sign and verify envelopes per §10 (or explicitly declare `auth: none`),
   agreeing on the canonical omission rules.
4. Expose and verify AgentCards per §7 (or run in local mode where the
   handshake provides the key).
5. Pass the §11.2 interop test against the reference implementation.

## 13. Bridges (ecosystem interop)

ACL does not replace existing protocols; it bridges to them.

### 13.1 ACL ↔ A2A (Agent2Agent v1.0)

Performatives map onto the A2A task lifecycle:

| ACL (performative)      | A2A TaskState     |
|-------------------------|-------------------|
| `request`, `delegate`   | `SUBMITTED`       |
| `progress`              | `WORKING`         |
| `input_required`        | `INPUT_REQUIRED`  |
| `inform` / Result       | `COMPLETED`       |
| `inform` / Text         | `WORKING`         |
| `failure`               | `FAILED`          |
| `cancel`                | `CANCELED`        |
| `reject`                | `REJECTED`        |

`acl_to_a2a_task` emits an A2A-shaped task object: `id` = `msg_id`,
`contextId` = `reply_with`, `artifacts` from Result artifacts, Failure
reason carried as the status message. `propose`/`accept`/`hello`/`bye` have
no A2A equivalent and are not mapped. Mapping is unit-tested in both
directions (`a2a_state_to_acl`).

### 13.2 ACL ↔ MCP (Model Context Protocol)

| ACL                             | MCP                                   |
|---------------------------------|---------------------------------------|
| AgentCard capabilities          | `tools/list` result (name, inputSchema) |
| `request` / Task{description, input} | `tools/call` {name, arguments}   |
| `inform` / Result{output}       | `tools/call` result {content, isError} |

A bridge agent presents itself as an ACL peer, performs the MCP `initialize`
handshake on the other side, and translates both directions — preserving ACL
signatures end-to-end on the ACL side (validated end-to-end against a real
MCP server). An MCP tool thus reachable appears in the bridge's AgentCard as
an ACL capability.

## 13bis. Local agent registry (additive)

Same-machine discovery without a directory service: a JSON file maps agent
names to `{aid, public_key, capabilities[], registered_at_ms}`. APIs:
`register` (upsert by name), `lookup(name)`, `by_capability(cap)`,
`list`, `unregister(name)`. The registry is advisory — a peer's AgentCard
(handshake) remains the trust root; the registry only caches where to find
peers. Reference: `acl_core::registry` + `registry_cli`.

## 14. Spec versioning and evolution (normative)

- The envelope field `acl` carries the **SPEC-VERSION** (currently `1`).
  Receivers MUST reject unknown major versions.
- **Major** bump: any change that breaks decoding or verification of v1
  messages. **Minor**: additive, backward-compatible (new performatives,
  new transports). **Patch**: clarifications only.
- Breaking changes REQUIRE an RFC (see `governance/`) with a migration
  path; dual-stack decoders are recommended during transitions.
- The changelog is authoritative for what changed between spec versions.

## 15. Implementation status (v0.2)

| component | tests / validation |
|-----------|--------------------|
| acl-core codec (L1–L5) | 11/11 unit tests (roundtrip, sign/tamper, framing, card) |
| spec-only Python codec | interop both directions with acl-core (§11.2) |
| `acl-py` binding | 5/5 (incl. cross-verification by the spec-only codec) |
| stdio transport (`acl-peer`) | live-pipe demo: §8 workflow + signed card |
| HTTP/SSE transport (`acl-httpd`) | live demo: sync POST, signed SSE stream, card |
| A2A bridge | unit-tested mapping + end-to-end vs real A2A server (HTTP) |
| MCP bridge (`acl-mcp-bridge`) | end-to-end against a real MCP server (node) |
| multi-agent demo (`tests/pilots/`) | 3 Python agents over live pipes: watch, input_required workflow, real delegate chain — all signed |
| WebSocket transport (`acl-wsd`) | live demo: RFC 6455 upgrade, binary frames, signed replies |
| sealed envelopes (`src/sealed.rs`) | roundtrip + wrong-key + tampered-ciphertext tests (§10.1) |
| idempotency (`src/idempotency.rs`) | dedup/violation/eviction tests (§8.1) |
| content language (`src/cl.rs`, §6.2) | roundtrip all ops, canonical ordering, grammar matrix, lifecycle; **live demo: acl-peer answers facts, executes actions, rejects ill-formed (422)** |
| TypeScript binding (`acl-ts`) | node test: sign/verify/decode/framing/tamper |
| local registry (`src/registry.rs`) | register/lookup/by-capability/persist tests + CLI |
| robustness (`tests/robustness.rs`) | 70k+ fuzz-lite inputs, decoder never panics; cargo-fuzz target in `fuzz/` |
| benchmarks (`tests/bench.rs`) | encode 0.57 µs, decode 0.94 µs, verify 80 µs; msgpack −43% vs JSON (BENCH.md) |
| IDL + compiler (§18, `src/idl.rs`, `aclc`) | 5/5 tests (parse/typecheck/errors/pretty-roundtrip/codegen); stdlib compiled + linted + gen smoke in CI |
| CL composition (§6.3) | nested and/or/not/ref roundtrip + grammar matrix recursive to leaves |
| epistemic facts (§6.4) | modality/provenance/confidence/validity roundtrip + backward-compat defaults |
| dialogue rules (§16, `src/dialogue.rs`) | full reply-matrix tests incl. bye-closes-session |
| formal semantics (§17, `src/semantics.rs`) | FP/RE present for all 14 performatives |
| runtime schema validation (§19, `src/schema.rs`) | 5/5 tests (valid/missing/wrong-type/extra-param/foreign-ns, alias resolve, composition recursion, commitment act) |
| declarative workflows (§20, IDL) | parse/typecheck/pretty-roundtrip/codegen tests; `do` targets validated against same-file actions |
| contract-net protocol (§21, `src/cnp.rs`) | full lifecycle (announce→bid→award→report), award-without-bid rejected, off-policy rejected |
| DID resolution (§22, `src/did.rs`) | 5/5 tests (roundtrip, malformed rejects, envelope-verify via did:key `from` field, did:web URLs) |
| batch verification (`src/batch.rs`) | 4/4 tests (all-valid, tampered-index, missing-sig, empty) |
| conformance suite (§23, `acl-conformance`) | 10/10 checks PASS, COMPLIANT level 3 |

Not yet implemented: sealed-envelope key rotation/HD derivation,
WS client demo in CI, public governance body.

## 19. Runtime schema validation (normative)

At build time, `aclc gen schema vocab.acl` emits `<ns>.schema.json`:
`{namespace, version, aliases, predicates, actions}` — aliases resolved to
primitives, params typed. At runtime, a receiver MAY load one or more
schemas and validate every `Cl` body BEFORE processing:

- predicate/action must exist in a loaded vocabulary (full name match);
- every declared param MUST be present and type-conformant
  (`string`→is_string, `int`→integer, `float`→number, `bool`→boolean,
  aliases resolved);
- undeclared params are rejected (strict);
- `Commitment.act` is validated as an action; composition recurses (§6.3);
- `ref` is skipped (validated where produced).

A receiver that validates SHOULD reject the message with `error` code 422
and the first violation as the reason. Reference: `src/schema.rs`.

## 20. Declarative workflows (IDL)

Workflows declare ordered action plans inside a vocabulary:

```text
workflow pipeline(text: string) -> bool {
  do acl.std.fs.file_read;
  parallel {
    do acl.std.text.a;
    do acl.std.text.b;
  }
  do acl.std.fs.file_write;
}
```

- Steps run in sequence; a `parallel { ... }` group's actions may run
  concurrently; groups nest no deeper than one level.
- Every `do` target MUST reference an action declared in the same file
  (full-name match). Unknown targets are type errors.
- `aclc` emits `plan_<name>()` builders returning the ordered groups,
  so executors can drive the plan without re-parsing the IDL.

## 21. Contract-Net Protocol (interaction pattern)

The standard negotiation pattern (Smith 1980) is a first-class ACL
interaction, built entirely on existing performatives and the CL:

| phase | message | content |
|-------|---------|---------|
| ANNOUNCE | manager → participants: `request` | `Cl` with the task `action` |
| BID | participant → manager: `propose` | `Cl` with `commitment{status=proposed}` |
| AWARD | manager → winner: `accept` | `Cl` with `commitment{status=active}` |
| AWARD (losers) | manager → others: `reject` | `Ref` |
| REPORT | winner → manager: `inform` | `Cl` with `fact` (or `failure`) |

Rules:
- Bids are commitments: awarding activates the winner's, losers' proposals
  are void.
- A manager MUST NOT award a contract to an agent that did not bid.
- The report closes the session: `fact{truth=true}` = fulfilled,
  `fact{truth=false}` or `failure` = breached/failed.
- Dialogue legality: `request → propose` is a legal reply (counter-offer);
  `propose → accept` activates.

Reference: `src/cnp.rs` (`CnpSession` tracks phases and validates
transitions).

## 22. DID resolution (identity layer)

The AID scheme `did:key` resolves OFFLINE to an Ed25519 verifying key:

```text
did:key:z<base58btc(0xed 0x01 || pubkey32)>
```

- `did_key_to_verifying_key` parses and validates the multicodec prefix;
- `verifying_key_to_did_key` encodes — an agent can derive its own AID
  from its keypair with no registry;
- envelope verification then needs ONLY the `from` field: no separate key
  exchange for did:key agents.

The `did:web` scheme resolves over HTTPS (`did:web:host[:port][:path]` →
`https://host[:port]/[path/]did.json`); fetching is the caller's transport
concern. Reference: `src/did.rs`.

## 23. Conformance suite

`acl-conformance` runs the certification checks and prints a report:

| level | checks |
|-------|--------|
| L1 | C1 codec roundtrip · C2 signature+tamper · C3 stream framing |
| L2 | C4 CL grammar · C5 dialogue rules · C6 idempotency |
| L3 | C7 sealed envelopes · C8 schema validation · C9 DID resolution · C10 batch verification |

Exit 0 = compliant at the highest fully-passing level. External
implementations MUST pass L1 to claim ACL support; L3 requires the full
feature set. Reference: `src/bin/acl_conformance.rs`.

## 24. Store-and-forward relay and delivery guarantees (normative)

A relay turns point-to-point ACL into an asynchronous system. Reference:
`src/bin/relay.rs`.

**Wire protocol** (all framed per §3):
1. client connects, sends any envelope; its `from` AID identifies its mailbox;
2. the relay resolves `did:key` senders (§22) and verifies with ONLY the
   `from` field; unresolvable schemes (e.g. `local:`) are accepted unsigned;
3. replay protection: duplicate `msg_id`s within a 5-minute window are
   rejected (`ReplayGuard`);
4. rate limiting: token bucket per connection (default 100 burst, 1 msg/2ms);
5. the envelope is enqueued for `to` — persistent on disk, one file per
   message — unless already expired (`ttl`, swept on delivery);
6. everything pending for the client's mailbox is delivered immediately,
   BATCH-VERIFIED first (§23 C10): messages whose signature fails are
   dropped before sending; the mailbox is drained after delivery.

**Delivery guarantees**: at-least-once while the recipient polls; messages
persist across relay restarts (file-backed store); expired messages are
dropped. Confidentiality: sealed envelopes (§10.1) stay sealed through the
relay — it routes opaque ciphertext.

## 25. Packaging and installability

- `acl-core` publishes to crates.io (metadata validated; `cargo publish
  --dry-run` passes). `cargo install acl-core` installs the `aclc` compiler.
- `acl-py` publishes to PyPI via maturin (`packaging/py/pyproject.toml`).
- `acl-ts` publishes to npm (`packaging/npm/package.json`).
- Package versions track the GitHub release tag; the wire protocol version
  is independent (`acl` field, §14).
- Publishing requires maintainer registry tokens; see PUBLISHING.md.

## 26. RDF / JSON-LD mapping (interop layer)

CL facts export as JSON-LD claims for the Linked-Data ecosystem
(reference: `src/rdf.rs`):

```json
{"@id": "urn:acl:claim:<msg_id>", "@type": "acl:Claim",
 "acl:assertedBy": {"@id": "<assertor AID>"},
 "acl:about": {"@id": "<base>/<vocab>/<predicate>"},
 "acl:holds": true, "acl:arg_<name>": <value>,
 "acl:modality": "believed", "acl:provenance": [...], "acl:validUntil": <ms>}
```

- dotted vocabulary names map to IRI paths: `acl.std.fs.file_exists` →
  `<base>/acl.std.fs/file_exists`;
- commitments map to `acl:Commitment` nodes with debtor/creditor/act/status;
- queries and actions are speech acts, not claims — skipped in the graph;
- the `@context` binds `acl:` to `https://acl-language.dev/ns/core/` and
  `vocab:` to the vocabulary's base.

This gives ACL facts W3C-graph semantics: any JSON-LD processor (and any
agent consuming schema.org-style data) can consume ACL claims.

## 27. Typed content values (normative)

CL argument values are TYPED: `ClValue = Str | Int | Float | Bool | Var |
List | Map | Bytes`. Wire-compatible with plain msgpack/JSON (Str→string,
Int→integer, …) so v0.1-v0.5 encoders keep working — but the type is now
carried at the language level:

- `Int` rejects floats: `5` is Int, `5.5` is Float (strict at decode);
- `null` is NOT a CL value (typed languages have no untyped bottom);
- `Var(name)` serializes as the single-key map `{"$var": name}` — an
  unambiguous encoding that cannot collide with `Map` (receivers check
  `$var` before treating a map as data);
- Rust-level consumers get the type for free (`value.type_name()`,
  `value.as_str()`), without re-parsing.

## 28. Bindings, partial application and quantification

**Bindings** — `bind(name, expr, body)`: `expr` must be a fact/action
leaf; `body` may reference it via `ref("$bind:<name>")`. The grammar
validator tracks the lexical scope: a `$bind:` reference to an unbound
name is ill-formed. Nesting is arbitrary (inner binds shadow outer).

**Partial application** — `Action` MAY declare `partial: [param...]`:
parameters deliberately left unfilled; the action is a template awaiting
completion. Schema validation (§19) skips params listed as partial —
every other declared param must still be present and typed.

**Quantification** — `for_all(var, over, such_that)` and
`exists(var, over, such_that)`: `over` MUST be a query (its result is the
collection); `such_that` follows the enclosing performative's grammar
with `var` bound (references via `{"$var": name}` in args). Quantifiers
are legal under `inform` and `query`.

**Metanível** — `believes(agent, fact)`: the sender asserts what ANOTHER
agent holds. Legal only under `inform`; the inner `fact` follows the fact
grammar. This is what lets agents reason about other agents' knowledge
(FIPA-SL tradition) without collapsing beliefs into facts.

## 29. Semantic time

- `Query.valid_until_ms` — the asker states the deadline after which the
  answer is worthless; receivers SHOULD discard late answers.
- `Renew(target_msg_id, new_deadline_ms)` — extends an ACTIVE
  commitment's deadline; legal only under `request`. Renewal is explicit
  (never implicit by re-creation).
- `Fact.derived_from(msg_id)` — the fact was computed from another
  message; its validity is bounded by the source's validity. Receivers
  SHOULD drop derived facts whose source has expired (§6.4).

## 30. Explicit intent and feasibility

The envelope gains two OPTIONAL semantic fields:

```text
rational_effect           # what the sender expects to happen (RE)
feasibility_precondition  # what must have been true of the sender (FP)
```

These mirror §17's formal semantics as first-class message data: a
sender that declares RE/FP makes its intent machine-checkable, and a
receiver can verify the message's declared effect against its own
capabilities BEFORE acting. Absent fields mean "§17 defaults apply".

## 31. Vocabulary evolution and deprecation

- `.acl` vocabularies MAY annotate declarations:
  `@deprecated("use file_v2 instead")` — parsed into `Deprecation
  {reason, since}` (since = vocabulary version).
- `aclc gen schema` emits a `deprecated` map (term → reason+version).
- `RuntimeSchema.validate_with_warnings` returns validation result PLUS
  deprecation warnings — deprecated terms still validate (no breakage)
  but clients are told.
- Rules: within a major version, deprecation is the ONLY allowed
  removal mechanism; major bumps (§14) MAY remove deprecated terms.

## 32. Expressiveness layer (normative)

Seven additions that make the language honest about knowledge, time,
conditionality and failure. All additive: performatives 15-17 extend the
§5 table; everything else is optional fields or new CL ops.

### 32.1 Belief revision — retract and supersedes

- New performative **`retract` (15)**: carries a `Ref` to a prior message.
  Legal reply after `inform` (matrix §16: `inform → {inform, error,
  retract}`). Semantics: the sender withdraws its earlier assertion —
  the fact is not deleted; receivers mark it retracted (provenance
  keeps the history).
- `Fact.supersedes(msg_id)`: the new fact REPLACES the fact asserted in
  the referenced message (e.g. `task_status` flips false). Retraction
  with traceability.

### 32.2 Subscriptions — push instead of poll

- New performatives **`subscribe` (16)** and **`unsubscribe` (17)**.
- `Body::Subscribe { watch: Matcher, until_ms? }` — ongoing interest.
- `Body::Unsubscribe { target_msg_id }` — cancels a prior subscription.
- **`Matcher`** (§32 construct): declarative message filter shared by
  subscriptions and routers — `performative?`, `from?`, `predicate?`.
- Dialogue: `subscribe → {inform, failure, error}`,
  `unsubscribe → {inform, error}`.

### 32.3 Conditional commitments

`conditional_commitment(debtor, creditor, condition, act, ...)` —
activates only when `condition` holds, as observed by the DEBTOR. The
condition is a fact composition (validated under inform's grammar);
acting as condition is ill-formed. Enables real negotiation: IF you do
X, THEN I commit to Y.

### 32.4 Evaluation policy — prefer

`prefer(criteria...)` — legal under `propose` (bid states its own
preference policy) and `accept` (award states its ranking policy).
Criteria are fact compositions; empty criteria ill-formed. CNP
evaluation becomes expressible in the language instead of hidden in
receiver code.

### 32.5 Structured error taxonomy

`Body::Error` gains `taxonomy: Option<ErrorKind>` — machine-actionable
failure kinds (snake_case on the wire): `unknown_predicate`,
`schema_violation`, `mailbox_full`, `rate_limited`, `unauthorized`,
`unknown_agent`, `decode_failed`, `expired`. Receivers react by kind,
not by parsing message strings.

### 32.6 Units of measure

`ClValue::Measured { value, unit }` — serializes as
`{"$measured": {"value": v, "unit": "ms"}}`, the same single-key-tag
convention as `$var` (§27). `5000 ms` is no longer confusable with
`5000 s`.

### 32.7 Semantics (§17 extension)

`retract`: FP = sender previously asserted p and now believes it false;
RE = receiver no longer holds p as asserted by sender.
`subscribe`: FP = sender wants change notifications until deadline;
RE = receiver pushes matching informs.
`unsubscribe`: FP = sender no longer wants the referenced subscription;
RE = receiver stops pushing.

## 33. Protocol programs (`.aclp`)

ACL becomes a PROGRAMMING LANGUAGE whose primitives are speech acts.
A protocol program turns the conversation itself into source code:
the compiled artifact is an agent that speaks ACL, with no imperative
code of its own.

### 33.1 Grammar (informative)

```text
protocol <name> v<n>;
roles <role>, ...;
state <name>[: <type>];            // session variables
on <trigger> -> <action>;          // one-line handler
on <trigger> { <action>; ... }     // multi-action handler
end when <builtin|state>;
```

Triggers: any performative (optionally patterned, `on request("quote")`),
`breach` (an owed commitment broke), `expired` (deadline passed).
Actions (phase 1): `send <performative>`, `begin` (activate pending
commitment), `fulfil`, `retract(target)` (§32.1 as a statement).

### 33.2 Well-formedness (normative)

- Roles: non-empty, unique.
- Handlers: at least one; no duplicate (trigger, pattern) pairs; no
  empty handler blocks.
- **Dialogue matrix at compile time**: every `send` inside a handler
  MUST satisfy §16 for its trigger — `on query -> send request` does
  not compile. `breach`/`expired` handlers may send only performatives
  legal after at least one plausible session opener (request, propose,
  delegate).
- `end when <state>` requires a declared state; `fulfilled`, `expired`,
  `breached` are builtins.
- Unknown performatives/triggers/statements are compile errors.

### 33.3 Semantics

Execution = the conversation. Each handler firing is a received speech
act; each `send`/`retract` is an emitted one. State variables are the
session state (§5); `begin`/`fulfil` transition the commitment lifecycle
(§6.2). Compile-time checking (§33.2) plus signed envelopes means the
execution log is an auditable transcript by construction.

### 33.4 Runtime (normative)

`Runtime(role)` executes a role's endpoint as a live agent:

- Input: `Event::In { from, performative, pattern? }`, `Event::Breach`,
  `Event::Expired`. Output: `OutEvent { to, performative }`.
- Handler selection: FIRST handler whose trigger matches (performative,
  pattern when both present, `from` when declared).
- Actions execute in declaration order: `send` emits, `begin` moves
  Proposed → Active, `fulfil` moves Active → Fulfilled, mis-sequenced
  transitions are logged and IGNORED (never fatal), `retract(t)` sets
  state `t` false (§32.1 semantics).
- **Auditability**: EVERY received event enters the transcript, handled
  or not; every emission and every state transition is logged with a
  sequence number. The transcript is the proof of protocol conformance.
- Session end: `end when` evaluates builtins (`fulfilled` → commitment
  is Fulfilled; `breached` → Breached) or a declared state being true.
- The runtime is a pure function of (program, delivered events) — no
  I/O, no clock: deterministic and replayable.

### 33.5 Multiparty projection (normative)

A global protocol COMPILES into per-role endpoints:

- A handler executes in the role that RECEIVES its trigger: `from R`
  ⇒ every role ≠ R; `breach`/`expired` execute in the role that is NOT
  the handler's send target (the debtor reports to the creditor).
- `send X to R2` emits in the executor's endpoint; omitted `to` defaults
  to the counterpart (two-role protocols).
- Projection requires exactly 2 roles in phase 3; roles must be declared;
  sends may not target the sender role.
- A role whose triggers are all attributed to itself gets an EMPTY
  endpoint — initiators act, they do not react.
- Two projected runtimes, driven through a wire, reproduce the global
  conversation: their combined transcripts contain every in/out event
  of the protocol, and the commitment lifecycle completes once.

## 34. Programming-language layer (normative)

Eight additions that complete `.aclp` as a programming language.

### 34.1 Guards

`on <trigger> when <guard>` — dispatch requires the guard to hold.
Grammar: `not` > `and` > `or`, atoms are BOOL session states. Compile
check: every referenced state must be declared with type `bool`
(non-bool or undeclared state = compile error). Unset states evaluate
false. Guards also gate timers.

### 34.2 Typed session states

`state <name>: bool|int|str;` — assignments are type-checked at runtime
(mismatches logged, never fatal); guards and `end when` require bool
states (compile time). Vocabulary types (e.g. `Money`) are accepted and
behave as `str` until typed vocabularies bind.

### 34.3 Content patterns

Handler patterns come in two forms: the literal `("quote")` (equality
with the event's pattern string) and the content pattern
`{"task": "deploy*"}` — a JSON object that must be a SUBSET of the
event's structured fields, with `*`-suffixed values matching by prefix.
An event without fields never matches a content pattern.

### 34.4 Time-driven handlers

`every <N>s|ms|m while <guard> as <role>` — fires periodically while
the guard holds, attributed to one role (unattributed = every role).
The runtime stays clock-free: the host calls `tick(elapsed_ms)`, due
timers fire deterministically, re-ticking the same elapsed never
double-fires.

### 34.5 Linearity (deadlock prevention at compile time)

EVERY send must have a receiver: the targeted role's projection must
contain a handler for the sent performative (bye excepted — it
terminates the session). Orphan send = compile error. In n>2 protocols,
triggers require `from` and sends require `to` (fully attributed).

### 34.6 Liveness (static deadlock warning)

Over the handler graph (edges: send → handlers listening for that
performative in the target role), every handler must reach a TERMINAL
action: `fulfil`, or a send of `bye`/`reject`/`error`. Unreachable =
warning (not error — external intervention may exist).

### 34.7 Endpoint codegen

`aclc gen agent py <file.aclp> <out_dir>` emits a RUNNABLE Python
skeleton per role: the projected handlers as data, guard evaluation,
content matching, commitment lifecycle, and timer ticking — a compiled
agent with zero ACL dependencies, wired to any transport by feeding
`deliver()`/`tick()`.

### 34.8 Transcript verification

`verify_transcript(prog, events)` replays a recorded exchange through
the projected runtimes: every emitted message must be dialogue-legal
for the receiving role's previous turn (§16), target a declared role,
and have a destination. Unhandled deliveries are warnings (logged,
visible) — never silent. Replay is deterministic and bounded.

## 35. Completing the language (normative)

Fifteen additions that make `.aclp` a complete programming language
for agents.

### 35.1 Expressions and `set`

`set <state> = <expr>;` — pure expressions over session states and the
triggering event's fields (`arg.<field>`). Operators: `+ - * /`,
`< <= > >= == !=`, `and or not` (precedence: not > mul > add > cmp >
and > or). Unset states behave as 0 in arithmetic. Builtins (all pure):
`add sub mul div min max abs len sum count concat upper lower str int
exists first last`.

### 35.2 Collections

Session states gain `list` and `map` types: `state bids: list;`.
Statements: `push <list>, <expr>;`, `put <map>, <key>, <expr>;`.

### 35.3 Message templates

`message <name> = "<text with {0}, {1}...>";` — named by role and
rendered with `fmt(name, args...)`. Templates are part of the compiled
program (code and presentation travel together).

### 35.4 Abort and compensation

`abort;` statement and top-level `abort when <guard>;` (checked after
every delivery, latched — compensation runs once). Aborting marks the
commitment breached and fires every `on breach` (compensation) handler
of the role. Programs declare failure handling, not just happy paths.

### 35.5 Declarative retry

`send X to Y retry <n> backoff <dur> [exp];` — the runtime tracks
pending sends and re-emits on host ticks; exponential doubling with
`exp`. Exhaustion is logged in the transcript (visible, never silent).

### 35.6 Capability model

`use <cap> [as <alias>];` declares what the program NEEDS. The language
itself grants no effects: a program can only speak and remember; any
external effect goes through a declared capability that the HOST gates
against the peer's AgentCard before the session starts.

### 35.7 Session templates (multi-session)

`capacity <n>;` bounds concurrent sessions. `SessionHost` multiplexes
sessions of one program with fully isolated state per session; opening
beyond capacity is rejected.

### 35.8 Event sourcing

Every delivered event is journaled (handled or not). `replay_from`
reconstructs a host from journal + role header — byte-equivalent state,
because the runtime is a pure function of (program, events). Crash
recovery and full audit come from the same mechanism.

### 35.9-35.11 n-role projection, transports, liveness

Projection extends beyond two roles when fully attributed. `transport
stdio|http|ws|relay;` declares the intended transport (unknown =
compile error). Liveness analysis is unchanged but now includes the new
action kinds.

### 35.12 Scenario tests

`scenario "<name>" { <role> sends <perf> [pattern] to <role>; expect
<role> sends <perf>; expect state <role>.<name>; expect commitment
<role> <status>; }` — executable tests inside the `.aclp` file, run on
the deterministic runtime via `aclc scenarios`. Expected sends DELIVER
to their target (the wire is simulated), so multi-step conversations
are testable end-to-end.

### 35.13 Protocol composition

`include "<file>" [with roles (A=R1, B=R2)];` — splices a foreign
protocol with role renaming (whole-word). Name collisions between
merged states are compile errors.

### 35.14 Standard fragments

`stdlib.rs` ships reusable handler fragments (`deadline_escalation`,
`double_confirmation`) with dedup on merge.

### 35.15 Tooling

`aclc scenarios <file>` runs all scenarios; `aclc trace <file.aclp>
<journal.jsonl>` is the protocol debugger — replays a journal step by
step and prints each role's transcript (seq, direction, text).

## 36. Closing the gaps (normative)

### 36.1 Capability enforcement (default-on)

The §35.6 declaration is ENFORCED, not decorative: `SessionHost` grants
nothing by default. A program with required capabilities cannot open a
session until the environment grants them (`new_gated(grants)`), and a
peer's AgentCard can be checked against the program's requirements
(`peer_satisfies`). `stdlib.*` names are library fragments, not
capabilities.

### 36.2 Language Server

`acl-lsp` — an LSP server for `.aclp` (stdio JSON-RPC):
`textDocument/publishDiagnostics` (parse + dialogue/linearity/liveness/
guard errors with line numbers, warnings for liveness), completion
(keywords, performatives, declared states and roles), full-sync
didOpen/didChange.

### 36.3 Protocol registry

File-backed registry with integrity: `aclc publish <file.aclp>
<registry.json> [publisher]` stores the source + SHA-256 checksum;
`aclc protocols <registry.json>` lists; `aclc fetch <name> <version>
<registry.json>` verifies the checksum before parsing — a tampered
registry fails to fetch.

## 37. Hardening and real-world readiness (normative)

### 37.1 HKDF key derivation (§10.1 hardening)

The raw X25519 shared secret is NEVER used as the ChaCha20-Poly1305
key. The symmetric key is derived: `HKDF-SHA256(salt, shared)`, expand
with info `acl-sealed-v1-key` (salt = the 12-byte message salt, domain-
separated from its nonce use by the info label).

### 37.2 Session key derivation

`derive_session_key(root_secret, session_id)` — derive per-channel keys
from a root secret via HKDF (info = `acl-session-v1-{session_id}`),
deterministic and distinct per session; no re-handshake per message.

### 37.3 Identity rotation

`Body::KeyRotation { new_did, valid_from_ms, reason? }` — a rotation
statement SIGNED BY THE OLD KEY: continuity is provable because the
message verifies under the old did:key while announcing the new one.
`rotate_identity(old_key, new_key, ...)` builds it.

### 37.4 Transport authentication

- `acl-httpd --token <t>`: bearer-token gate (`Authorization: Bearer`)
  on every ACL-carrying route; `/agent-card` stays open (capability
  discovery is verifiable pre-auth). Unauthorized = 401.
- `acl-wsd --token <t>`: token via upgrade-path query param.

### 37.5 Operations vocabulary

`stdlib/agent_ops.acl` — the vocabulary for operating AI agents:
`agent_healthy`, `agent_status`, `heartbeat`, `tool_call`, `tool_result`,
`telemetry`, `delegation_active`, `delegate_task`, `run_tool`,
`rotate_identity`, `sla_acknowledged`.

### 37.6 Reference protocol program

`stdlib/protocols/pilot_ops.aclp` — orchestrator delegates a tool call
to a pilot with an SLA commitment (delegate → propose → accept → begin
→ tool_result → fulfil), with an embedded passing scenario.

### 37.7 Soak verification

The multi-session host is soak-verified: 200 concurrent sessions × 25
events, full-journal replay reconstructs exact state, outbox accounted
(one propose per session).

### 37.8 Registry ↔ AgentCard discovery

`Registry::register_card(&AgentCard)` and `Registry::discover_cards(cap)`
— capability discovery returns best-effort AgentCards with public keys
restored from the registry (signature verification deferred to contact).

### 37.9 Dialogue-matrix enforcement (§16 runtime)

`Runtime::enforce_dialogue(true)` — incoming messages are validated
against the dialogue matrix given the last outgoing performative;
off-policy replies are logged ("off-policy reply dropped") and the
handler does not fire. Opt-in per runtime.

### 37.10 Bridges completed

- `a2a_task_to_acl(task)` — the inverse A2A bridge: full task JSON →
  Envelope (performative from state, body from artifacts/metadata,
  correlation preserved). Roundtrip ACL→A2A→ACL tested.
- `validate_arguments(card, name, args)` — JSON-schema-lite validation
  of MCP call arguments against the card's declared input schema
  (required fields + types).

## 16. Dialogue rules (normative)

A conversation has policy: after a given performative, only certain replies
are legal. `dialogue_legal(prev, reply)` (reference: `src/dialogue.rs`):

| prev | legal replies |
|------|---------------|
| `request` / `delegate` | `inform`, `input_required`, `failure`, `reject`, `propose` (counter-offer, §21), `progress`, `error` |
| `query` | `inform`, `failure`, `error` |
| `propose` | `accept`, `reject`, `error` |
| `accept` | `inform`, `failure`, `error` |
| `cancel` | `inform`, `error` |
| `inform` | `inform`, `error` |
| `hello` | `hello`, `error` |
| `input_required` | `inform`, `cancel`, `error` |
| `progress` | `progress`, `inform`, `failure`, `input_required`, `error` |
| `failure` / `reject` | `inform`, `error` |
| `bye` | (none — session closed) |
| `error` | `inform`, `error` |

Receivers SHOULD reject off-policy replies; strict sessions MAY close the
session on violation. A `query` MUST eventually be answered by a `fact`
(possibly negated) — never by `accept` or `progress`.

## 17. Formal semantics per performative

Every performative carries machine-readable **feasibility preconditions**
(FP: what must hold of the sender's mental state) and **rational effects**
(RE: the intended effect on the receiver), FIPA-ACL style. Reference:
`src/semantics.rs`. Examples:

```text
FP(inform): sender believes proposition(p) is true; sender intends receiver to believe it
RE(inform): receiver believes proposition(p)

FP(accept): a prior commitment exists with status=proposed and sender is its creditor
RE(accept): commitment becomes active; debtor owes the act

FP(failure): receiver attempted act(a) and could not complete it; failure is terminal
RE(failure): sender knows act(a) failed (and why); no result will come
```

All 14 performatives have FP/RE annotations; agents and verifiers SHOULD
process them rather than prose. Sincerity: a sender violating an FP is
insincere — receivers MAY downgrade trust in the sender's future `believed`
facts.

## 18. IDL and toolchain

Vocabularies are declared in `.acl` IDL files and compiled by `aclc`:

```text
namespace acl.std.fs v1;

type Path = string;

/// doc comment attached to the next declaration
predicate file_exists(path: Path) -> bool;
action file_read(path: Path) -> string;
```

- Full name = `<namespace>.<local>` (e.g. `acl.std.fs.file_exists`) —
  namespaces disambiguate vendor vocabularies.
- Primitives: `string int float bool bytes timestamp any`; aliases via
  `type X = primitive;`.
- **aclc** subcommands: `check` (parse+typecheck), `lint` (+ doc-comment
  policy), `fmt` (canonical pretty-print; idempotent; the human-readable
  surface syntax), `gen rust|py|ts` (typed builder stubs for ClExpr),
  `names` (list fully-qualified names).
- The **standard library** ships in `stdlib/`: `fs.acl`, `task.acl`,
  `time.acl`, `net.acl` (namespace `acl.std.*`). CI compiles them on every
  push.
- Generated builders produce CL expressions bound to the declared,
  type-checked names — the vocabulary contract travels with the code.

## 38. Formal grammar (normative)

The complete grammar of the language — lexical tokens, program
structure, handlers, actions, expressions, guards, scenarios, and
top-level declarations — is normatively defined in Extended Backus-Naur
Form (EBNF) in [GRAMMAR.md](GRAMMAR.md). The recursive-descent parser in
`src/protocol.rs` implements that grammar; when the parser and the
grammar disagree, the grammar wins (or the grammar has a bug — then fix
the grammar and cite the evidence).

The grammar also defines:

- **Scopes and resolution** — global (session states), function-local
  (parameters + locals, shadowing globals), handler (captures shadow
  states), and loop scope (§38.9 of GRAMMAR.md).
- **Semantic rules** — dialogue matrix (E014), linearity (E015),
  liveness (W201), guard types (E020), type checking (E021), function
  return (E030), capability gate (E040).

Error codes are normative: implementations must use the same codes for
the same classes of violation, so that diagnostics are portable.

## 39. Protocol language — guards, types, fan-out, templates, modules (normative)

This section completes the language layer of §33-§35 and §38. The
implementations live in `src/protocol.rs` (parse/check) and
`src/runtime.rs` (evaluation); the grammar productions are in
[GRAMMAR.md](GRAMMAR.md).

### 39.1 Expression guards and typed captures

A guard that references a pattern capture or uses a comparison operator
is a full expression, evaluated at delivery time with the trigger's
captures in scope; captures shadow session states. Capture values are
type-inferred from the payload (int, float, bool, else string), so
`on inform {"price": $p} when $p > 100 -> ...` compares numerically. A
bare `$var` value inside a content pattern is normalized to the JSON
string `"$var"` at parse time.

### 39.2 Trigger alternation

`on (accept | reject) from buyer` expands at parse time into one handler
per alternative with identical guard and actions. Only the one-line and
inline-block forms accept alternation; a block-form alternation is a
compile error.

### 39.3 Handler sets (fragments)

`handler_set name { ... }` declares a reusable handler fragment;
`include handler_set name;` splices its handlers into the current
protocol. Roles referenced by the fragment are checked against the
including protocol's roles at include time. A set may be declared once
per file (duplicate = compile error).

### 39.4 Static typing

`static_ty` infers the static type of an expression (literals, declared
state types, record fields, user function return types, arithmetic and
comparison operators). Assignment to a typed state with a known
incompatible type is a compile error; `int` widens to `float`; captures
and `arg.*` are statically unknown and keep the runtime checks of §35.1.
Record field access (`q.price`) and record literals (`Quote { price: 5 }`)
are validated against the declared `type` — unknown fields and field
access on scalars are compile errors.

### 39.5 Errors as flow

Internal action failures — `set`/`push`/`put` evaluation errors,
non-list/non-map container violations, and send retry exhaustion — are
routed to the session's unattributed `on error` handler as a synthetic
Error event with the failure text in the `$reason` capture. The failure
is always logged; failures inside the error handler itself are logged
only (recursion is cut off — no error loops). `on error from X` handlers
receive only external Error messages from role X.

### 39.6 Broadcast and bid selection

`broadcast <perf> [payload] to <list_state>;` emits one send per role
named in the list state (contract-net announce). Dialogue legality is
enforced exactly like an ordinary send; the target must be a declared
state. `prefer(list, key)` selects the map or record with the LOWEST
numeric value for `key` (empty list yields unset) — the bid-award
primitive for contract-net.

### 39.7 Rich template interpolation

`fmt` accepts either a registered template name or an inline template
containing `${expr}` interpolations — full expressions (states,
captures, arithmetic, record fields) evaluated in the caller's scope.
`$${` is the literal escape for `${`. Registered templates late-bind
their `${expr}` interpolations at the call site and keep positional
`{0}`-style placeholders. Unmatched `${` is an evaluation error.

### 39.8 Modules and visibility

`export handler_set name;` marks a fragment importable; everything else
in a module file stays private. `import "file.aclp" as ns;` loads the
module (with its own includes/imports, depth-limited to 8 — import
cycles are errors) and splices ONLY the exported sets, namespaced as
`ns.<set>`; without an alias the original names are used and name
collisions between imports are compile errors. `include handler_set
ns.name;` then splices the imported fragment into the protocol.

The `aclc` tool resolves includes and imports relative to the edited
file's own directory, for every subcommand that reads a `.aclp`.

## 40. Hardened static validation (normative)

### 40.1 Expression guards are compile-checked
A guard that is a full expression (§39.1) is validated at compile time:
states it references must be declared; comparisons whose operands have
statically known incompatible types are compile errors. Captures and
`arg.*` remain statically unknown and keep the runtime checks.

### 40.2 State type lattice
Declared state types are validated: `bool | int | str | float | list |
map | list<T> | map<K,V> | <record>`, where T, K, V are scalars or
declared record types (K also `int`). Runtime assignments into typed
states, and `push`/`put` into generic containers, are checked per
element; a violation routes to the session's `on error` handler (§39.5)
as an internal failure.

### 40.3 Source attribution
Every handler carries its source line; compile diagnostics for handlers
point at the `on` declaration instead of a synthetic position.

## 41. Protocol inheritance and vocabulary export (normative)

`protocol v2 extends v1;` (same file) inherits roles, states, types,
functions, templates, scenarios, `end when`, transport, capacity and
`abort_when` from the base. A handler in the derived protocol with the
SAME trigger + guard signature OVERRIDES the base handler; other base
handlers are inherited unchanged. Multiple `protocol` declarations in
one file MUST form an `extends` chain.

`export type X;` and `export fn name;` mark vocabulary as importable;
`import "file.aclp" [as ns];` splices exported types and functions under
their own names (they are referenced from expressions, where a namespace
prefix would not parse) — a name collision is a compile error. Handler
sets splice namespaced as `ns.<set>` (§39.8).

## 42. n-role projection (normative)

`project()` supports any role count. In protocols with more than two
roles, `check` requires explicit `from` on triggers and `to` on sends
(§34); the counterpart default exists ONLY in two-role protocols, and a
send without `to` in an n-role protocol is a projection error. A trigger
may restrict its executor to a single role with `on X from R as <role>`;
the executor must be a declared role and differs from the sender.

## 43. Declarative await (normative)

`await N <perf> into <list> within <dur> as <flag>;` arms a collector:
subsequent matching incoming events are CONSUMED by the collector (they
do not reach handlers) and their payload maps are appended to the list
state. When N payloads have arrived, `<flag>` (a declared bool state)
becomes true; when the deadline passes first, tick() sets it false and
logs `await timed out`. The flag starts false when the await is armed.

## 44. Local error flow (normative)

`try { <actions> } catch { <actions> };` — a failing action inside the
body (evaluation errors, container violations, `fmt` failures) cuts the
remaining body short and runs the catch actions. Failures INSIDE the
catch are not caught by the same try — they route to the session's
`on error` handler (§39.5). `on X ... -> <action> handle error ->
<action>;` is sugar for a single-action try/catch.

## 45. Typed fields on the wire (normative)

`Event::In.fields` carries typed values (string, int, float, bool).
String literals convert implicitly; native numeric/boolean payloads pass
through WITHOUT the string-inference heuristic of §39.1 (a string
payload may still be a number written as text — it is inferred). Literal
and prefix-wildcard patterns compare by text rendering. The `ClValue`
content language (§27) converts to wire field values.

## 46. Toolchain obligations (normative)

- `aclc fmt <file.aclp>` emits the canonical formatting of a program
  (all declarations; import lines are resolved by the loader and are NOT
  preserved — `fmt` refuses files that contain them). Formatting is
  idempotent.
- Scenario steps accept `expect log "<substring>";` — satisfied when any
  role's transcript contains the substring.
- A user function that recurses (directly or mutually) is a compile
  error: the runtime has no call stacks.
