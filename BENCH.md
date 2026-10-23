# ACL codec benchmarks

Measured with `tests/bench.rs` (std::time loop, 50k iterations after warmup),
release build, Linux x64 (Rust 1.95, ed25519-dalek 3, rmp-serde 1.3).
Envelope: request/Task with JSON input (~300 bytes on the wire).

## Throughput

| operation              | ops/s       | per-op      |
|------------------------|-------------|-------------|
| encode (msgpack)       | ~1,744,000  | 0.57 µs     |
| decode (msgpack)       | ~1,063,000  | 0.94 µs     |
| sign (ed25519)         | ~26,400     | 37.9 µs     |
| verify (ed25519)       | ~12,400     | 80.5 µs     |

Reads:

- **Codec cost is negligible.** Encode+decode of a full signed envelope is
  under 1.5 µs combined; a single Ed25519 verify costs ~50–80× more. For any
  agent whose thinking time is >100 ms, protocol overhead is unmeasurable.
- **Verify dominates by design** — asymmetric crypto is the price of
  identity; batch verification (ed25519-dalek batched APIs) is a v0.2 option
  for high-fanout relays.
- To reproduce: `cargo test --release --test bench -- --ignored --nocapture`.

## Wire size

| encoding   | bytes (same envelope) |
|------------|-----------------------|
| msgpack    | 302                   |
| JSON       | 532 (**−43%** with msgpack) |

Cross-checked by `compactness` unit test (asserts msgpack < JSON).

## ACL vs natural language (same communicative act)

Measured with `scratch` script: the SAME message (request: quarterly
report from s3, PDF, 3 sections, 24h deadline) rendered both as an ACL
envelope (encoded by the **independent spec-only codec**, `py_codec.py`)
and as the natural-language paragraph an agent would have to generate
and another agent's LLM would have to parse. Tokens via tiktoken
o200k_base.

| metric            | ACL (msgpack)       | natural language        |
|-------------------|---------------------|-------------------------|
| wire bytes        | 372                 | 431 (only 1.2×)         |
| LLM tokens        | **0**               | 100 per message         |
| interpretation    | 6 µs (deterministic decode, python) | 22 µs tokenize **+ 1 full LLM round-trip** |
| LLM calls / msg   | **0**               | 2 (generate + parse)    |

8-message contract-net negotiation (1 announce, 5 bids, 1 award, 1 report):

| resource         | ACL              | natural language                 |
|------------------|------------------|----------------------------------|
| bytes            | 2,976            | 3,448                            |
| LLM calls        | **0**            | 16                               |
| LLM tokens       | 0                | 800 out + 800 in                 |
| wall-clock       | ~ms (network-bound) | ~10–18 s serialized †         |
| cost (gpt-4o-mini)† | $0            | ~$0.0006 (800×$0.60/M + 800×$0.15/M) |
| cost (frontier, $2.5/$10)† | $0     | ~$0.012                          |
| determinism      | exact (byte-comparable, signed) | probabilistic — same input can parse differently |

† CITED, not measured: OpenAI gpt-4o-mini list pricing $0.15/M input,
$0.60/M output; P50 latency ~0.6 s, E2E ~1.1–3.5 s per call
(openrouter.ai, infron.ai — Oct 2025). Token/byte figures ARE measured.

**Honest reading:** bytes are NOT the story (1.2×). The real costs of
natural language are (a) 2 LLM calls per message — latency in seconds,
cost linear in message volume, (b) non-determinism: "within 24 hours"
must be re-parsed and re-pinned by an LLM every time, while
`deadline_ms=86400000` is pinned once, signed, and machine-checkable,
and (c) auditability: commitments, deadlines and provenance are fields
you can verify, not prose you must re-interpret.

At personal scale the dollar difference is negligible; at agent-fleet
scale (10k msgs/day = 20k LLM calls) NL adds ~hours of cumulative
latency and $0.75–$8+/day purely for encoding/decoding meaning. ACL
makes the *social* layer (who promised what, by when) deterministic and
free of inference; natural language stays inside payloads where it
belongs.

Method note: reproduce with tiktoken o200k_base + `tests/interop/
py_codec.py`; NL text is the minimal faithful rendering of the same
task fields — longer politeness strategies only increase the NL column.
