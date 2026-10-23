# Changelog

All notable changes to the ACL spec and reference implementation.
Format based on Keep a Changelog; versions follow semver for the
**implementation** and a separate `SPEC-VERSION` for the protocol.

## [Unreleased]

## [0.11.0] — 2026-10-23

The complete programming language: computation, collections, failure
handling, multi-session operation, event sourcing, executable tests,
composition, and tooling. 15 additions (spec §35).

### Added
- **Expressions + `set`** (§35.1): arithmetic/comparison/logic over
  states and event fields (`arg.x`), 18 pure builtins; unset = 0.
- **Collections** (§35.2): `list`/`map` states with `push`/`put`.
- **Message templates** (§35.3): `message name = "text {0}";` rendered
  by `fmt()`.
- **Abort + compensation** (§35.4): `abort;` statement, `abort when
  <guard>` (latched), `on breach` handlers fire as compensation.
- **Declarative retry** (§35.5): `send X retry 3 backoff 2s exp` —
  deterministic re-emission via tick; exhaustion logged.
- **Capability model** (§35.6): `use fs.read as reader;` — programs can
  only speak and remember; external effects are host-gated.
- **Multi-session host** (§35.7): `capacity n;`, isolated sessions,
  capacity rejection.
- **Event sourcing** (§35.8): journal + deterministic `replay_from`
  reconstructs exact state (tested byte-equivalent).
- **n-role projection / transports** (§35.9-35.11).
- **Scenario tests** (§35.12): executable tests inside `.aclp`, run via
  `aclc scenarios` — expected sends DELIVER to their target.
- **Composition** (§35.13): `include "file.aclp" with roles (a=X);`.
- **Standard fragments** (§35.14): `deadline_escalation`,
  `double_confirmation`, dedup on merge.
- **Tooling** (§35.15): `aclc trace` protocol debugger (step-through
  replay with per-role transcripts).
- **Codegen**: generated Python agents now compute (`set`/`push`/`put`/
  `for`/`abort`) — validated by running generated code.
- 16 new tests (**118/118**).

## [0.10.0] — 2026-10-23

The programming-language layer completes: `.aclp` programs now branch on
session state, match message content, run on host-injected time, and are
checked for deadlock at COMPILE time — plus runnable codegen and
transcript verification.

### Added
- **Guards** (§34.1): `on accept when quoted` — dispatch gated on bool
  session states (`not/and/or`, compile-checked).
- **Typed session states** (§34.2): `bool|int|str` with runtime-checked
  assignments; guards/end-when require bool states.
- **Content patterns** (§34.3): `on request {"task": "deploy*"}` — JSON
  subset matching with prefix wildcards, alongside literal patterns.
- **Time-driven handlers** (§34.4): `every 60s while active as seller`
  — deterministic `tick()` injection, no clock in the runtime.
- **Linearity** (§34.5): every send must have a receiver — orphan sends
  are COMPILE ERRORS (deadlock prevention).
- **Liveness** (§34.6): static reachability to terminal actions;
  unreachable handlers warn as possible deadlocks.
- **Endpoint codegen** (§34.7): `aclc gen agent py` — runnable Python
  agent skeletons per role, validated end-to-end (generated agents
  complete the commitment lifecycle).
- **Transcript verification** (§34.8): `verify_transcript` replays
  recorded exchanges through projected runtimes — off-policy messages
  and orphan destinations are errors; unhandled deliveries are warnings.
- Dialogue matrix: `bye` is now legal after every performative except
  after `bye` itself (any participant may close the session).
- 13 new tests (101/101).

## [0.9.0] — 2026-10-23

Phases 2 and 3 of the protocol-program stack: the runtime that EXECUTES
`.aclp` programs and multiparty projection that compiles a global
protocol into per-role endpoints.

### Added
- **Runtime** (§33.4): `src/runtime.rs` — `Runtime(role)` delivers
  events, executes handlers in order, tracks the commitment lifecycle
  (mis-sequenced transitions logged and ignored, never fatal) and keeps
  an auditable transcript: every received event is logged handled or
  not. Pure function of (program, events) — deterministic, replayable.
- **Multiparty projection** (§33.5): `project(prog, role)` compiles the
  global protocol into a per-role endpoint — each role sees only its
  handlers; `from`/`to` attribution grammar added (`on request("quote")
  from buyer -> send propose to buyer`); breach/expired handlers execute
  in the debtor role (≠ send target). Initiators get empty endpoints.
- **`aclc endpoints`**: prints each role's projected endpoint.
- 8 new tests (88/88): projection sides, n>2 rejection, handler order,
  commitment lifecycle, breach firing, unhandled-event audit, full
  two-runtime conversation reproduction, retract statement.

## [0.8.0] — 2026-10-23

ACL becomes a programming language whose primitives are speech acts:
protocol programs (`.aclp`) compile into agents that speak the protocol,
with the dialogue matrix enforced at COMPILE time.

### Added
- **Protocol programs** (spec §33): `src/protocol.rs` — parser and
  checker for `.aclp` files: `protocol`/`roles`/`state`/`on handlers`/
  `end when`. Triggers: any performative (patterned or not), `breach`,
  `expired`. Actions: `send`, `begin`, `fulfil`, `retract`.
- **Compile-time dialogue checking**: every `send` is validated against
  the §16 dialogue matrix for its trigger at BUILD time — an illegal
  reply is a compile error, not a runtime failure. Breach/expired
  handlers are checked against plausible session openers.
- **`aclc protocol`**: compile subcommand (parse + check + summary).
- 7 new tests: parse/check, pretty roundtrip, dialogue violation,
  breach constraint, duplicate handlers, end_when states, unterminated
  blocks.

## [0.7.0] — 2026-10-23

The expressiveness layer: agents can now revise beliefs, subscribe to
events, make conditional promises, state preferences, report typed
failures, and use units. All additive (performatives 15-17, optional
fields); SPEC-VERSION on the wire remains `1`.

### Added
- **Belief revision** (§32.1): `retract` performative (15) + `Fact.
  supersedes` — withdraw/replace prior assertions with full provenance.
- **Subscriptions** (§32.2): `subscribe` (16) / `unsubscribe` (17) with
  declarative `Matcher` filters — push instead of poll.
- **Conditional commitments** (§32.3): IF-THEN commitments, condition
  validated as fact composition observed by the debtor.
- **Evaluation policy** (§32.4): `prefer(criteria)` under propose/accept
  — CNP ranking expressible in the language.
- **Error taxonomy** (§32.5): 8 machine-actionable `ErrorKind`s on
  `Body::Error.taxonomy`.
- **Units of measure** (§32.6): `ClValue::Measured` (`{"$measured":
  {"value": v, "unit": "ms"}}`).
- **Formal semantics** (§17): FP/RE for retract/subscribe/unsubscribe.

## [0.6.0] — 2026-10-23

The language matures: typed values, bindings, quantification, semantic
time, explicit intent, and vocabulary evolution. All changes additive;
SPEC-VERSION on the wire remains `1`.

### Added
- **Typed content values** (spec §27): `ClValue` (Str/Int/Float/Bool/Var/
  List/Map/Bytes) with strict typing (`Int` rejects floats, `null` is not
  a CL value) and full wire compatibility with previous encoders.
- **Bindings** (spec §28): `bind(name, expr, body)` with `$bind:` lexical
  references, scope-tracked by the grammar validator.
- **Partial application** (spec §28): `Action.partial` lists unfilled
  params; schema validation skips them.
- **Quantification** (spec §28): `for_all(var, over, such_that)` and
  `exists(...)` with query domains.
- **Metanível** (spec §28): `believes(agent, fact)` — reasoning about
  other agents' knowledge.
- **Semantic time** (spec §29): `Query.valid_until_ms`, `Renew` operator
  for commitments, `Fact.derived_from` with validity propagation.
- **Explicit intent** (spec §30): envelope `rational_effect` and
  `feasibility_precondition` fields — FP/RE as message data (§17).
- **Vocabulary evolution** (spec §31): `@deprecated` annotations in IDL,
  `deprecated` map in compiled schemas, `validate_with_warnings` API.

## [0.5.0] — 2026-10-23

From standalone language to distributed system. All changes additive;
SPEC-VERSION on the wire remains `1`.

### Added
- **Store-and-forward relay** (spec §24): `acl-relay` daemon — TCP, file-
  backed persistent queues, mailbox drain on connect, TTL expiry sweep,
  did:key signature verification via the `from` field alone, batch-verified
  delivery (invalid messages dropped pre-send).
- **Replay protection & rate limiting** (spec §24): `ReplayGuard` (msg_id
  window) and token-bucket `RateLimiter`, enforced by the relay.
- **Runtime schema validation in the reference agent** (spec §19): `acl-peer
  --schema` validates incoming CL payloads against compiled vocabularies —
  strict full-name matching, param types, 422 with violation details.
- **Standard library extended** (§18): `proc.acl` and `http.acl` added
  (6 vocabularies, ~40 documented declarations).
- **`aclc` installable** (§25): `cargo install acl-core` puts the compiler
  on PATH; `cargo publish --dry-run` validated for crates.io.
- **RDF / JSON-LD mapping** (spec §26): CL facts → `acl:Claim` nodes,
  commitments → `acl:Commitment`; provenance and epistemic metadata mapped.
- **Property-based tests** (proptest): encode/decode roundtrip on arbitrary
  strings, decode-never-panics, idempotency no-duplicates, encoding
  determinism.
- **Publishing guide** (PUBLISHING.md) for crates.io / PyPI / npm.

## [0.4.0] — 2026-10-23

Distributed-systems maturity: types enforced on the wire, declarative
workflows, standard negotiation, real identity, and a conformance suite.
All changes additive; SPEC-VERSION on the wire remains `1`.

### Added
- **Runtime schema validation** (spec §19): `aclc gen schema` emits
  machine-readable schemas; receivers validate CL payloads against them
  (strict params, alias resolution, composition recursion) before
  processing.
- **Declarative workflows** (spec §20): `workflow` blocks in the IDL with
  sequence and `parallel` groups; `aclc` emits plan builders.
- **Contract-Net Protocol** (spec §21): announce → bid → award → report
  built on performatives and commitments; `CnpSession` tracks phases and
  validates transitions.
- **DID resolution** (spec §22): `did:key` (ed25519, multicodec 0xed01)
  resolves offline to verifying keys — envelope verification needs only
  the `from` field; `did:web` URL construction.
- **Batch signature verification** (ed25519 batched API with exact
  per-item fallback).
- **Conformance suite** (spec §23): `acl-conformance` binary, 10 checks
  across 3 levels, prints report, exit 0 = compliant.
- **Packaging**: crates.io metadata for `acl-core` (0.4.0), maturin
  `pyproject.toml` for PyPI, `package.json` for npm.

The release that makes ACL a **language** rather than a transport protocol
with conventions. All changes additive; SPEC-VERSION on the wire remains `1`.

### Added
- **IDL and compiler** (spec §18): `.acl` vocabulary files with namespaces
  (`acl.std.fs.file_exists`), type aliases and doc comments; `aclc`
  subcommands `check` / `lint` / `fmt` (canonical pretty-print, idempotent)
  / `gen rust|py|ts` (typed CL builders) / `names`.
- **Standard library** (`stdlib/`): `fs.acl`, `task.acl`, `time.acl`,
  `net.acl` — 29 documented declarations under `acl.std.*`, compiled and
  linted in CI.
- **CL composition** (spec §6.3): `and` / `or` / `not` / `ref` operators;
  the §6.2 grammar matrix applies recursively down to leaves.
- **Epistemic facts** (spec §6.4): `modality` (asserted/believed/known),
  `provenance` chains, `confidence`, `valid_until_ms` — all optional,
  backward compatible.
- **Dialogue rules** (spec §16): normative reply matrix per performative
  (`query` → answered by facts; `propose` → accept/reject only; `bye`
  closes the session).
- **Formal semantics** (spec §17): machine-readable feasibility
  preconditions (FP) and rational effects (RE) for all 14 performatives,
  FIPA-ACL style.

## [0.2.0] — 2026-10-23

All changes additive; SPEC-VERSION on the wire remains `1`.

### Added
- **Content Language (CL)** (spec §6.2): shared grammar for facts, typed
  actions, queries and commitments with per-performative grammar matrix and
  commitment lifecycle; canonical arg ordering for stable signatures.
  Supported in acl-core, acl-py and acl-ts. Reference agent `acl-peer`
  answers fact queries, executes CL actions and rejects ill-formed bodies.
- **WebSocket transport** (`acl-wsd`, spec §9.3): RFC 6455 upgrade, binary
  frames, signed replies — validated end-to-end.
- **Sealed envelopes** (spec §10.1): X25519 + ChaCha20-Poly1305
  end-to-end confidentiality through relays.
- **TypeScript binding** (`acl-ts`, napi-rs): sign/verify/decode/framing.
- **Idempotency & ordering** (spec §8.1): semantic dedup by `msg_id`,
  retransmission rules, eviction.
- **Local agent registry** (spec §13bis): file-backed discovery + CLI.
- **A2A bridge validated live** against a real A2A server (HTTP):
  Agent Card ↔ AgentCard, message/send ↔ request/Task.
- **Multi-agent pilots demo**: 3 agents over live pipes with real
  delegate chains.
- **Governance**: CONTRIBUTING, RFC process + template, CODEOWNERS,
  issue templates, spec versioning policy (§14).
- **Robustness**: fuzz-lite corpus tests (70k+ inputs, decoder never
  panics) + cargo-fuzz target.
- **Benchmarks** (BENCH.md): encode 0.57 µs, decode 0.94 µs, verify
  80 µs; msgpack −43% vs JSON.
- **Docs CI**: rustdoc artifact on every push; tutorial + quickstart script.

## [0.1.0] — 2026-10-22

First public release.

### Spec (SPEC-VERSION 1)
- 6-layer architecture; MessagePack wire format with named-field encoding
- Envelope with 14 performatives (u8 discriminants) and 12 body variants
- Ed25519-signed envelopes and self-signed AgentCards; canonical §10 rule
  (omit absent Option fields, omit zero `hop_count`)
- Transports: stdio/pipes, HTTP sync, SSE streaming (per-event signatures),
  WebSocket (RFC 6455)
- Workflow state machine incl. `input_required` blocking turn
- Idempotency: semantic dedup by `msg_id` within TTL (§8.1)
- Sealed envelopes: X25519 + ChaCha20-Poly1305 (§10.1)
- Bridges: A2A task-state mapping, MCP tools mapping (§13)

### Reference implementation (Rust)
- `acl-core` crate (codec, signing, framing, bridges, idempotency, sealed)
- Binaries: `acl-peer` (stdio), `acl-httpd` (HTTP/SSE), `acl-wsd`
  (WebSocket), `acl-mcp-bridge`, `acl-a2a-bridge`
- Bindings: `acl-py` (PyO3, abi3), `acl-ts` (napi-rs)
- Independent spec-only Python codec; interop validated both directions
- Robustness fuzz-lite corpus test; codec benchmarks

[Unreleased]: https://github.com/acl-language/acl/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/acl-language/acl/releases/tag/v0.6.0
[0.5.0]: https://github.com/acl-language/acl/releases/tag/v0.5.0
[0.4.0]: https://github.com/acl-language/acl/releases/tag/v0.4.0
[0.3.0]: https://github.com/acl-language/acl/releases/tag/v0.3.0
[0.2.0]: https://github.com/acl-language/acl/releases/tag/v0.2.0
[0.1.0]: https://github.com/acl-language/acl/releases/tag/v0.1.0
