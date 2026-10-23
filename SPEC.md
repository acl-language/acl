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

## 11. Reference Implementation

`acl-core` (Rust) implements L1–L4 of this spec:
- MessagePack encode/decode of `Envelope`
- Ed25519 sign/verify
- `AgentCard` struct + signing
- Streaming frame codec (4-byte length header)

Language bindings planned: Python (PyO3), TypeScript (napi-rs).

## 12. Compliance

An implementation is ACL-0.1 compliant if it can:
1. Encode/decode every performative in §5 with its body variant from §6.
2. Frame messages on at least one transport from §9.
3. Sign and verify envelopes per §10 (or explicitly declare `auth: none`).
4. Expose and verify AgentCards per §7 (or run in local mode where the
   handshake provides the key).
