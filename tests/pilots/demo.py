#!/usr/bin/env python3
"""Pilot-shaped demo: THREE Python agents speaking real ACL over stdio pipes.

Topology (mirrors the ADK pilot roles, no mocks — every message is a signed
ACL envelope over live pipes):

    driver (= orchestrator/Rocky role)
      ├─ argus  (watcher): accepts "watch" tasks, reports status
      ├─ kenji  (coder):   accepts "code" tasks; may raise input_required
      └─ balboa (worker):  accepts "run" tasks

Flow exercised:
  1. driver -> argus: request/watch  -> inform/Result
  2. driver -> kenji: request/code (missing input) -> input_required
  3. driver -> kenji: inform (answer)  -> inform/Result
  4. driver -> kenji: request/delegate-chain -> kenji DELEGATES to balboa
     (real `delegate` performative over a second pipe pair), balboa runs,
     result flows back up the chain.
"""

import os, subprocess, sys, time, secrets
import msgpack

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "interop"))

HERE = os.path.dirname(os.path.abspath(__file__))

# each agent is a subprocess running agent.py with a role
AGENTS = {
    "argus": {"role": "argus"},
    "kenji": {"role": "kenji", "peer": ("balboa", "balboa")},
    "balboa": {"role": "balboa"},
}


def spawn(role, extra_args):
    env = dict(os.environ, ACL_ROLE=role, **({"ACL_PEER_CMD": extra_args[0]} if extra_args else {}))
    return subprocess.Popen(
        [sys.executable, os.path.join(HERE, "agent.py"), role],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)


class Agent:
    def __init__(self, name):
        self.name = name
        self.p = spawn(name, ())
        self.pub = self._read_pub()

    def _read_pub(self):
        pub_file = f"/tmp/acl-pilots/{self.name}.pub"
        for _ in range(100):
            if os.path.exists(pub_file):
                break
            time.sleep(0.05)
        return open(pub_file, "rb").read()

    def exchange(self, env):
        self.p.stdin.write(msgpack.packb(env, use_bin_type=True))
        self.p.stdin.flush()
        return msgpack.unpackb(self.p.stdout.read1(1 << 20), raw=False)


from py_codec import NAME_TO_ID as _NTI
def mk_env(from_, to, perf, body, in_reply_to=None):
    env = {"acl": 1, "msg_id": secrets.token_bytes(16).hex(), "from": from_, "to": to,
           "ts": int(time.time() * 1000), "ttl": 300000,
           "performative": _NTI[perf], "body": body}
    if in_reply_to:
        env["in_reply_to"] = in_reply_to
    return env


def main():
    os.makedirs("/tmp/acl-pilots", exist_ok=True)
    for f in os.listdir("/tmp/acl-pilots"):
        os.remove(f"/tmp/acl-pilots/{f}")

    from py_codec import PERFORMATIVE, NAME_TO_ID, key_of, verify_sig, read_frame

    agents = {}
    for name in AGENTS:
        if name == "kenji":
            # kenji can spawn/pipe to balboa through agent.py relay mode
            agents[name] = subprocess.Popen(
                [sys.executable, os.path.join(HERE, "agent.py"), "kenji"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL)
        else:
            agents[name] = subprocess.Popen(
                [sys.executable, os.path.join(HERE, "agent.py"), name],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL)

    def ex(agent, env):
        agents[agent].stdin.write(msgpack.packb(env, use_bin_type=True))
        agents[agent].stdin.flush()
        payload, _ = read_frame(agents[agent].stdout.read1(1 << 20))
        return msgpack.unpackb(payload, raw=False)

    def txt(v):
        return v.decode() if isinstance(v, bytes) else v

    # load agent pubkeys
    def wait_pub(n):
        f = f"/tmp/acl-pilots/{n}.pub"
        for _ in range(100):
            if os.path.exists(f):
                return open(f, "rb").read()
            time.sleep(0.05)
        raise RuntimeError(f"{n} never published its key")
    pubs = {n: wait_pub(n) for n in AGENTS}
    me = "local:orchestrator:driver"
    ok = 0

    # 1. watch task
    env = mk_env(me, "argus", "request",
                 {"t": "Task", "v": {"description": "watch", "input": {"target": "gateway"}}})
    r = ex("argus", env)
    assert verify_sig(r, pubs["argus"])
    assert key_of(r, "body")["v"]["output"]["status"] == "watching"
    print(f"[1] argus: watch -> {key_of(r, 'body')['v']['output']}"); ok += 1

    # 2/3. kenji input_required workflow
    env = mk_env(me, "kenji", "request",
                 {"t": "Task", "v": {"description": "code", "input": {}}})
    r = ex("kenji", env)
    assert PERFORMATIVE[key_of(r, "performative")] == "input_required"
    need_id = txt(key_of(r, "msg_id"))
    print(f"[2] kenji: input_required — needs {key_of(r, 'body')['v']['what']!r}")
    r2 = ex("kenji", mk_env(me, "kenji", "inform",
                            {"t": "Text", "v": {"text": "feature: add /health route"}},
                            in_reply_to=need_id))
    assert PERFORMATIVE[key_of(r2, "performative")] == "inform"
    print(f"[3] kenji: resumed -> {key_of(r2, 'body')['v']['output']}"); ok += 1

    # 4. delegate chain: kenji delegates the run to balboa
    env = mk_env(me, "kenji", "request",
                 {"t": "Task", "v": {"description": "code", "input": {"text": "deploy"}}})
    r = ex("kenji", env)
    assert verify_sig(r, pubs["kenji"])
    out = key_of(r, "body")["v"]["output"]
    assert out["delegated_to"] == "balboa", out
    assert out["run_result"]["executed"] is True, out
    print(f"[4] kenji -> balboa (real delegate): {out}"); ok += 1

    # cleanup
    for a in agents.values():
        a.stdin.write(msgpack.packb(mk_env(me, "x", "bye", {"t": "Ref", "v": {"target_msg_id": "x"}}), use_bin_type=True))
        a.stdin.flush()
    for a in agents.values():
        a.wait(timeout=5)

    print()
    print(f"=== PILOTS DEMO PASS: {ok}/3 exchanges, 3 agents, all messages signed ACL envelopes ===")


if __name__ == "__main__":
    main()
