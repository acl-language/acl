# Publishing ACL packages

The packaging metadata is validated (`cargo publish --dry-run` passes).
Actual publication requires registry tokens owned by the maintainers.

## crates.io — `acl-core`

```bash
# 1. create a token at https://crates.io/settings/tokens
# 2. login (stores in ~/.cargo/credentials)
cargo login <TOKEN>
# 3. publish (dry-run first)
cargo publish --dry-run
cargo publish
# 4. verify: https://crates.io/crates/acl-core
```

After publication, users get the compiler + library with:

```bash
cargo install acl-core   # installs the `aclc` compiler binary
```

## PyPI — `acl-py`

```bash
# 1. build the wheel (requires maturin: pip install maturin)
cd packaging/py
maturin build --release
# 2. upload (requires a PyPI token: https://pypi.org/manage/account/token/)
twine upload ../target/wheels/acl_py-*.whl
# users install with: pip install acl-py
```

## npm — `acl-ts`

```bash
cd packaging/npm
npm login
npm publish
# users install with: npm install acl-ts
```

## Version policy

Crate/package versions track the GitHub release tag (v0.4.0 → 0.4.0).
The protocol version on the wire is independent (`acl` field, currently 1).
