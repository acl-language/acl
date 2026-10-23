# ACL — Agent Communication Language

**Version:** 0.1.0-draft
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
  recipient's public key; the 32-byte shared secret is the ChaCha20-Poly1305
  key directly (v0.1 simplification; HKDF extraction is a v0.2 TODO).
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
  `agent.card` queries, handles `bye`.
- `acl-httpd` — HTTP + SSE server (§9.1): `POST /`, `GET /events`,
  `GET /agent-card`; per-event signatures on the SSE stream.
- `acl-mcp-bridge` — translates between ACL and a real MCP server
  (JSON-RPC over stdio): `tools/call` ↔ `request`/Task, `tools/list` ↔
  signed AgentCard.

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

## 15. Implementation status (v0.1)

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

Not yet implemented: WebSocket transport, sealed envelopes (v0.2),
TypeScript binding, live-network A2A peer validation, public governance.
