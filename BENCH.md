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
