# RFC process

Protocol changes go through RFCs, adapted from the Rust/Python model but
deliberately lightweight.

## When you need an RFC

Any change that affects:
- the wire format (fields, encoding, framing),
- performatives or their semantics,
- security properties (signature rules, identity),
- backward compatibility.

Bug fixes, docs, tests, and bindings do **not** need an RFC.

## Lifecycle

```
draft  →  review (min. 1 week, min. 2 reviewers)  →  accepted | rejected
                                                        ↓
                                              implemented + interop-tested
                                                        ↓
                                                spec version bump
```

1. Open a PR with `rfcs/NNN-title.md` (template: `rfc-template.md`).
2. Discuss in the PR; update the document in place.
3. Accepted RFCs merge into `rfcs/` and their changes land in `SPEC.md`
   in a follow-up PR referencing the RFC number.
4. Breaking changes bump the spec major version; the `acl` field in the
   envelope version-gates on the wire.

## Numbering

Sequential: `001-ws-transport.md`, `002-sealed-envelopes.md`, …

## Template

See [rfc-template.md](rfc-template.md).
