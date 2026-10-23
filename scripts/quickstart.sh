#!/usr/bin/env bash
# ACL quickstart — two agents talking in ~10 minutes.
# Run from the repo root: scripts/quickstart.sh
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== 1/5 build =="
cargo build --bin interop --bin peer --quiet

echo "== 2/5 python venv (once) =="
if [ ! -d tests/interop/.venv ]; then
  python3 -m venv tests/interop/.venv
  tests/interop/.venv/bin/pip -q install msgpack cryptography websockets
fi
PY=tests/interop/.venv/bin/python3

echo "== 3/5 unit tests =="
cargo test --quiet 2>&1 | grep "test result"

echo "== 4/5 interop: spec-only codec <-> Rust core =="
mkdir -p /tmp/acl-quickstart
rm -f /tmp/acl-quickstart/*
$PY tests/interop/py_codec.py /tmp/acl-quickstart "$(pwd)/target/debug/interop" 2>&1 | tail -1

echo "== 5/5 two live agents over pipes =="
rm -rf /tmp/acl-quickstart-peer
mkdir -p /tmp/acl-quickstart-peer
$PY tests/interop/stdio_demo.py /tmp/acl-quickstart-peer "$(pwd)/target/debug/peer" 2>&1 | tail -1

echo
echo "Done. Next steps:"
echo "  - SPEC.md                      (the protocol)"
echo "  - tests/pilots/demo.py         (three agents, delegate chains)"
echo "  - tests/mcp/bridge_demo.py     (talk to MCP servers)"
echo "  - acl-py / acl-ts              (embed in your agent)"
