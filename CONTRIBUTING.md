# Contributing to ACL

Thanks for considering a contribution. ACL is a **programming language**
for AI agents: the spec defines the language, the compiler enforces it,
the runtime executes it. Most changes will touch `SPEC.md`, the compiler
(`src/protocol.rs`, `src/bin/aclc.rs`) or the runtime (`src/runtime.rs`,
`src/session.rs`).

## The golden rule

**Spec first.** If an implementation and the spec disagree, the spec wins
(or the spec has a bug — then fix the spec and cite the evidence). Every
ambiguity found by cross-implementation interop is a spec bug; the fix
MUST land in `SPEC.md`, not just in code.

## Process

1. **Behavior changes to the wire format or the language grammar require
   an RFC** (see `governance/rfc-template.md`). Open a PR prefixed
   `[RFC-N]` describing motivation, wire impact, and backward
   compatibility. The wire format is additive-only within SPEC-VERSION 1.
2. **Bug fixes and tests** can be ordinary PRs.
3. All PRs must keep CI green:
   - unit tests (currently 130)
   - the spec-only interop test (§11.2) — an independent implementation
     written from SPEC.md alone must keep interoperating byte-exactly
   - the IDL stdlib check + lint (every vocabulary declaration needs a
     doc comment)
   - all transport demos and bindings builds (`acl-py`, `acl-ts`)

## Where changes land

| change | files | note |
|---|---|---|
| new performative | `SPEC.md` §5, `src/lib.rs`, `src/dialogue.rs`, bindings | update the dialogue matrix AND both bindings' exhaustive matches |
| new `.aclp` statement | `SPEC.md` §33+, `src/protocol.rs`, `src/runtime.rs`, `src/codegen.rs` | the generated Python agents must be updated too |
| new semantic check | `src/protocol.rs` `check()`, `SPEC.md` | compile errors or warnings, with tests |
| new transport | `SPEC.md` §9, new `src/bin/`, `.github/workflows/ci.yml` | add an interop demo |
| spec-only | `SPEC.md` | still needs a failing-test-first rationale |

## Adding a new implementation

An implementation is "conformant" when it passes §12 compliance. The
strongest evidence is a codec written **only from SPEC.md** that exchanges
signed messages with an existing implementation — if you do this, open a
PR adding your test to `tests/interop/`; it becomes part of CI.
`tests/interop/py_codec.py` is the working example.

## Language-design contributions

The highest-value contributions are things that make programs *safer*:

- new compile-time checks (the pattern: a compile error or warning that
  catches a class of agent bugs before deployment)
- new runtime guarantees (determinism, auditability, replay)
- better error messages (compile errors should name the violated rule)

## Commit style

One logical change per commit, message explains *why*, not just *what*.

## License

Apache-2.0. By contributing you agree your contributions are licensed
under it.
