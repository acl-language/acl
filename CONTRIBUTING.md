# Contributing to ACL

Thanks for considering a contribution. ACL is a protocol spec first, code
second — most changes will touch `SPEC.md`.

## The golden rule

**Spec first.** If an implementation and the spec disagree, the spec wins
(or the spec has a bug — then fix the spec and cite the evidence). Every
ambiguity found by cross-implementation interop is a spec bug; the fix MUST
land in `SPEC.md`, not just in code.

## Process

1. **Behavior changes to the wire format require an RFC** (see
   `governance/rfc-template.md`). Open a PR prefixed `[RFC-N]` describing
   motivation, wire impact, and backward compatibility.
2. **Bug fixes and tests** can be ordinary PRs.
3. All PRs must keep CI green: unit tests, the spec-only interop test
   (§11.2), and all transport demos.

## Adding a new implementation

An implementation is "conformant" when it passes §12 compliance. The
strongest evidence is a codec written **only from SPEC.md** that exchanges
signed messages with an existing implementation — if you do this, open a PR
adding your test to `tests/interop/`; it becomes part of CI.

## Commit style

One logical change per commit, message explains *why*, not just *what*.

## License

Apache-2.0. By contributing you agree your contributions are licensed
under it.
