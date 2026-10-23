#!/usr/bin/env python3
"""CL end-to-end demo: a real Rust agent (acl-peer) SPEAKS the content
language — answers fact queries about the filesystem, executes typed
actions, enforces the §6.2 grammar, and refuses ill-formed bodies.

  1. query/Cl(file_exists /etc/hostname)  -> inform/Cl(fact truth=true)
  2. query/Cl(file_exists /nope)          -> inform/Cl(fact truth=false)
  3. request/Cl(action text.reverse)      -> inform/Cl(fact text_reversed)
  4. query/Cl carrying an ACTION          -> Error 422 (grammar violation)
  5. request/Cl unknown action            -> Error 400
"""

import os, subprocess, sys, time, secrets
import msgpack

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame, write_frame


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def main():
    workdir = sys.argv[1]
    peer_bin = sys.argv[2]
    pub_file = os.path.join(workdir, "cl-peer.pub")
    if os.path.exists(pub_file):
        os.remove(pub_file)

    p = subprocess.Popen([peer_bin, "--name", "cl-peer", "--pub-out", pub_file],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL)
    for _ in range(100):
        if os.path.exists(pub_file):
            break
        time.sleep(0.05)
    pub = open(pub_file, "rb").read()
    me = "local:pyclient:cl"
    peer_aid = "local:cl-peer"

    def exchange(env):
        p.stdin.write(write_frame(msgpack.packb(env, use_bin_type=True)))
        p.stdin.flush()
        payload, _ = read_frame(p.stdout.read1(1 << 20))
        return msgpack.unpackb(payload, raw=False)

    def mk(perf, body):
        return {"acl": 1, "msg_id": secrets.token_bytes(16).hex(),
                "from": me, "to": peer_aid,
                "ts": int(time.time() * 1000), "ttl": 300000,
                "performative": NAME_TO_ID[perf], "body": body}

    def cl(exprs):
        return {"t": "Cl", "v": {"expressions": exprs}}

    # 1. fact query — path that exists
    r = exchange(mk("query", cl([
        {"op": "query", "predicate": "file_exists", "args": {"path": "/etc/hostname"}}])))
    assert verify_sig(r, pub), "sig invalid"
    assert PERFORMATIVE[key_of(r, "performative")] == "inform"
    f = key_of(r, "body")["v"]["expressions"][0]
    assert f["op"] == "fact" and f["predicate"] == "file_exists" and f["truth"] is True, f
    print(f"[1] peer asserts: {f['predicate']}({f['args']}) = {f['truth']}")

    # 2. fact query — path that does NOT exist
    r = exchange(mk("query", cl([
        {"op": "query", "predicate": "file_exists", "args": {"path": "/no/such/file"}}])))
    f = key_of(r, "body")["v"]["expressions"][0]
    assert f["truth"] is False, f
    print(f"[2] peer asserts: {f['predicate']}({f['args']}) = {f['truth']}")

    # 3. action execution
    r = exchange(mk("request", cl([
        {"op": "action", "name": "text.reverse", "params": {"text": "content-language"}}])))
    assert verify_sig(r, pub)
    f = key_of(r, "body")["v"]["expressions"][0]
    assert f["predicate"] == "text_reversed", f
    assert f["args"]["output"] == "egaugnal-tnetnoc", f
    assert f["truth"] is True
    print(f"[3] peer executed action -> fact: {f['predicate']}({f['args']})")

    # 4. grammar enforcement: query carrying an ACTION op = ill-formed
    r = exchange(mk("query", cl([
        {"op": "action", "name": "text.reverse", "params": {"text": "x"}}])))
    assert PERFORMATIVE[key_of(r, "performative")] == "error", r
    err = key_of(r, "body")["v"]
    assert err["code"] == 422 and "ill-formed" in err["message"], err
    print(f"[4] grammar enforced: {err['code']} — {err['message']}")

    # 5. unknown action
    r = exchange(mk("request", cl([
        {"op": "action", "name": "quantum.teleport", "params": {}}])))
    err = key_of(r, "body")["v"]
    assert err["code"] == 400 and "unknown action" in err["message"], err
    print(f"[5] unknown action rejected: {err['message']}")

    p.stdin.close()
    p.wait(timeout=5)
    print()
    print("=== CL AGENT DEMO PASS: Rust agent speaks the content language (§6.2) ===")


if __name__ == "__main__":
    main()
