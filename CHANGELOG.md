# Changelog

All notable changes to the ACL spec and reference implementation.
Format based on Keep a Changelog; versions follow semver for the
**implementation** and a separate `SPEC-VERSION` for the protocol.

## [Unreleased]

## [0.18.0] — 2026-10-23

### Added
- **Ordered session-open replay (§53)**: opens are journaled in order;
  replay respects capacity limits and session-ID reincarnations; legacy
  journals fall back to the role header.
- **Formatter obligations (§54)**: `pretty` emits phase declarations and
  `as <role>` attribution; send payloads before targets; new roundtrip
  test proves parse→pretty→parse is a semantic fixpoint and formatting
  is idempotent for a program using every feature.
- **Phase reachability warning**: a declared phase that is never
  `enter`-ed warns as unreachable.

### Fixed
- **Python single dispatcher (§54)**: the generated runtime had TWO
  action dispatchers that drifted — `enter`, `begin <id>` and
  interpolated sends silently did nothing inside `try`/`catch`. One
  dispatcher now serves top-level and nested execution.
- **Formatter payload order**: `action_word` emitted send payloads after
  the target, so re-parsing swallowed them into the target role.
- **Python `true`/`false` literals**: generated code emitted Rust
  lowercase booleans (NameError at load) instead of `True`/`False`.
- **Phase peel lost the `from` clause**: `on inform in work from b`
  reparsed with `from: None`.

### CI
- The Rust/Python parity harness runs explicitly in CI
  (`cargo test --test py_parity -- --ignored`).

## [0.17.0] — 2026-10-23

### Added
- **Compiler coverage (§47)**: checks recurse into nested actions
  (`for`/`try`/`catch`); payload correlation warnings (receiver pattern
  keys vs sent keys).
- **Replay completeness (§48)**: host ticks, session closes and
  capability grants are journaled; exact replay covers timers, awaits,
  retries and lifecycle.
- **Conversation phases (§49)**: `phase <name>;`, handlers gated with
  `in <phase>`, transitions with `enter <phase>;` — phases are
  instance-scoped.
- **Named commitments (§50)**: `begin <id>;` / `fulfil <id>;` with
  independent lifecycles per session.
- **Send payload interpolation**: `send inform {"r": "${y}"};`
  interpolates full expressions over session state (shared with the
  fmt engine).
- **Semantic parity harness (§51)**: `tests/py_parity.rs` drives the
  same inputs through the Rust runtime and the generated Python agent
  and asserts identical observable behavior.
- **Concurrent conversations (§52)**: instance-scoped states, phases
  and commitments — documented and tested.

### Fixed
- **Try executed with an empty scope**: nested `set x = $n * 2`
  evaluated captures as unset (0) inside `try`/`catch`/`for` bodies —
  `run_actions` now threads captures explicitly.

## [0.16.0] — 2026-10-23

### Added
- **Complete static validation (§40)**: expression guards compile-checked
  (undeclared states, incompatible comparisons); state type lattice
  (`float`, `list<T>`, `map<K,V>`, records) validated; runtime typed
  assignments checked per element (routed to `on error`); handler source
  lines in diagnostics.
- **Protocol inheritance (§41)**: `protocol v2 extends v1;` — inherits
  everything, overrides handlers by trigger+guard signature;
  `export type/fn` join handler sets as importable vocabulary (collision
  = compile error; duplicate declarations rejected).
- **n-role projection (§42)**: any role count; explicit attribution
  enforced; `on X from R as <role>` pins the executor; 3-role relay runs
  end-to-end through projected runtimes.
- **Declarative await (§43)**: `await N <perf> into <list> within <dur>
  as <flag>;` — collector consumes matching events, flag completes on
  count or times out; mirrored in Python codegen (E2E validated).
- **Local error flow (§44)**: `try { ... } catch { ... };` cuts the body
  and runs the catch; failures inside a catch route to `on error`;
  `handle error` one-line sugar.
- **Typed wire fields (§45)**: `FieldValue` (str/int/float/bool) on
  `Event::In` — native payloads skip string inference; string literals
  convert implicitly (all construction sites keep compiling);
  `From<ClValue>` bridge.
- **Toolchain (§46)**: `aclc fmt` canonical idempotent formatter (all
  declaration kinds; refuses files with imports); scenario `expect log`;
  function recursion (direct or mutual) is a compile error.

### Fixed
- Statement splitting broke `try { ... }` blocks (brace-aware split).
- Brace-counting underflow (usize) in handler_set parsing.
- Python `try`/indentation emission in generated agents.

## [0.15.0] — 2026-10-23

### Added
- **Expression guards + typed captures (§39.1)**: `when $p > 100` —
  guards are full expressions evaluated with the trigger's captures in
  scope; capture values are type-inferred (int/float/bool/string).
- **Trigger alternation (§39.2)**: `on (accept | reject) from buyer`
  expands to one handler per alternative.
- **Handler sets (§39.3)**: `handler_set name { ... }` fragments spliced
  via `include handler_set name;`, roles checked at include time.
- **Static typing (§39.4)**: compile-time assignment type checks (int
  widens to float; captures/args stay dynamic), record field access and
  record literals validated against declared types; record literals
  `Type { field: value }` are now parseable.
- **Errors as flow (§39.5)**: internal failures (set/push/put, retry
  exhaustion) route to `on error` with `$reason`, recursion-guarded.
- **Contract-net (§39.6)**: `broadcast <perf> to <list>;` fan-out +
  `prefer(list, key)` lowest-bid selection.
- **Rich templates (§39.7)**: `fmt("x ${expr}")` inline interpolation
  with `$${` escape; registered templates late-bind `${expr}` and keep
  `{0}` positional placeholders.
- **Modules (§39.8)**: `export handler_set name;` + `import "f.aclp" as
  ns;` — only exported sets splice, namespaced, collision detection,
  cycle-guarded recursion; `aclc` resolves includes/imports relative to
  the program file.
- Python codegen: guards/captures/broadcast/prefer/templates supported
  in generated agents (validated by running the generated code).

### Fixed
- Tokenizer zero-progress loop on `$` capture tokens (hung tests).
- Bare `$var` in content patterns produced invalid JSON — normalized to
  `"$var"` at parse time.
- Inline handler blocks split the block body before trigger parsing
  (payload braces no longer leak into the `from` clause).
- Python emission used `null` instead of `None` for unset `set` fields.

## [0.14.0] — 2026-10-23

### Added
- **Formal grammar** (`GRAMMAR.md`): complete EBNF definition of the
  language — lexical tokens, program structure, handlers, actions,
  expressions, guards, scenarios, scopes, and semantic rules with
  normative error codes (E014/E015/E020/E021/E030/E040, W201).
- **Spec §38**: the grammar is now normative — parser and grammar must
  agree; disagreements are spec bugs.
- **Language features** (implemented in v0.13.x cycle, now documented):
  user-defined functions (§38.1), record types with field access and
  literals (§38.2), pattern matching with variable capture (`$var`).

## [0.13.0] — 2026-10-23

Hardening and real-world readiness: the security TODOs are closed and
the protocol is wired for operating real agents.

### Added
- **HKDF-SHA256 key derivation** (§37.1): raw shared secrets never used
  as symmetric keys (closes the §10.1 v0.1 simplification).
- **Session key derivation** (§37.2): per-channel keys via HKDF.
- **Identity rotation** (§37.3): `Body::KeyRotation` signed by the old
  key — provable continuity; `rotate_identity` helper.
- **Transport authentication** (§37.4): bearer-token gate on
  `acl-httpd`, token on `acl-wsd` upgrade; agent-card stays pre-auth.
- **Operations vocabulary** (§37.5): `stdlib/agent_ops.acl` — telemetry,
  tool calls, delegation, heartbeats, SLA.
- **Reference protocol** (§37.6): `stdlib/protocols/pilot_ops.aclp`
  with a passing embedded scenario.
- **Soak verification** (§37.7): 200 sessions × 25 events, exact-state
  replay.
- **Registry ↔ AgentCard discovery** (§37.8): register/discover via
  cards.
- **Dialogue-matrix enforcement** (§37.9): opt-in strict mode on the
  runtime; off-policy replies logged and dropped.
- **Bridges completed** (§37.10): `a2a_task_to_acl` inverse bridge +
  MCP `validate_arguments` schema-lite validation.
- **Architecture diagram**: `docs/architecture.html` — visual stack map.

## [0.12.0] — 2026-10-23

The three remaining gaps closed: enforced capability gating, a language
server, and a tamper-evident protocol registry.

### Added
- **Capability enforcement, default-on** (§36.1): `SessionHost` grants
  nothing by default — a program with `use` declarations cannot open
  sessions until granted (`new_gated`); peer AgentCards verified via
  `peer_satisfies`. stdlib.* fragments are exempt.
- **`acl-lsp`** (§36.2): LSP server for `.aclp` — publishDiagnostics
  (parse + all semantic checks, line numbers), completion (keywords,
  performatives, declared states/roles). Validated end-to-end over
  stdio JSON-RPC framing.
- **Protocol registry** (§36.3): `aclc publish|protocols|fetch` —
  file-backed, SHA-256 checksummed source; fetch verifies integrity
  before parsing (tamper detection tested).
- 3 new tests (**122/122**).

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
