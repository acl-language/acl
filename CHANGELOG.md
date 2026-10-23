# Changelog

All notable changes to the ACL spec and reference implementation.
Format based on Keep a Changelog; versions follow semver for the
**implementation** and a separate `SPEC-VERSION` for the protocol.

## [Unreleased]

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

[Unreleased]: https://github.com/mrschneider/acl/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mrschneider/acl/releases/tag/v0.1.0
