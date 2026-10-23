#!/usr/bin/env python3
"""A generic ACL agent for the pilots demo (tests/pilots/agent.py <role>).

Speaks ACL over stdio (framed envelopes), signs everything with its own
Ed25519 key (published to /tmp/acl-pilots/<role>.pub at startup).

Roles:
  argus  — watcher: request/Task{target} -> Result{status: watching}
  balboa — worker:  request/Task{text}   -> Result{executed: true, ran: text}
  kenji  — coder:   request/Task without input.text -> input_required;
                    with input.text == "deploy" -> DELEGATES to balboa
                    (spawns agent.py balboa as a subprocess, real
                     delegate performative over a second pipe pair);
                    other texts -> Result{reversed}
"""

import os, subprocess, sys, time, secrets
import msgpack
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "interop"))
from py_codec import NAME_TO_ID, verify_sig, read_frame, write_frame

ROLE = sys.argv[1] if len(sys.argv) > 1 else "generic"
PERF = {v: k for k, v in NAME_TO_ID.items()}
FIELD_ORDER = ["acl", "msg_id", "in_reply_to", "from", "to", "ts", "ttl",
               "reply_with", "hop_count", "performative", "body"]


def txt(v):
    return v.decode() if isinstance(v, bytes) else v


def canonical(env):
    return {f: env[f] for f in FIELD_ORDER if f in env and not (f == "hop_count" and env[f] == 0)}


priv = Ed25519PrivateKey.generate()
pub = priv.public_key().public_bytes_raw()
os.makedirs("/tmp/acl-pilots", exist_ok=True)
pub_path = os.environ.get("ACL_PUB_PATH", f"/tmp/acl-pilots/{ROLE}.pub")
open(pub_path, "wb").write(pub)
my_aid = f"local:{os.getpid()}:{ROLE}"

print(f"[{ROLE}] ready", file=sys.stderr)


def sign_reply(env, perf, body):
    reply = {"acl": 1, "msg_id": secrets.token_bytes(16).hex(),
             "in_reply_to": txt(env["msg_id"]),
             "from": my_aid, "to": env["from"],
             "ts": int(time.time() * 1000), "ttl": 300000,
             "performative": NAME_TO_ID[perf], "body": body}
    reply["sig"] = priv.sign(msgpack.packb(canonical(reply), use_bin_type=True))
    return reply


def send(reply):
    sys.stdout.buffer.write(write_frame(msgpack.packb(reply, use_bin_type=True)))
    sys.stdout.buffer.flush()


def delegate_to_balboa(text, orig_env):
    """Spawn a balboa subprocess and delegate the task for real."""
    bal_pub_path = f"/tmp/acl-pilots/balboa-{os.getpid()}-{time.time_ns()}.pub"
    bal = subprocess.Popen([sys.executable, os.path.abspath(__file__), "balboa"],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=None,
                           env=dict(os.environ, ACL_PUB_PATH=bal_pub_path))
    task_env = {"acl": 1, "msg_id": secrets.token_bytes(16).hex(),
                "from": my_aid, "to": "local:child:balboa",
                "ts": int(time.time() * 1000), "ttl": 300000,
                "performative": NAME_TO_ID["delegate"],
                "body": {"t": "Delegate", "v": {
                    "chain": [my_aid],
                    "task": {"t": "Task", "v": {"description": "run", "input": {"text": text}}}}}}
    bal.stdin.write(msgpack.packb(task_env, use_bin_type=True))
    bal.stdin.flush()
    payload, _ = read_frame(bal.stdout.read1(1 << 20))
    reply = msgpack.unpackb(payload, raw=False)
    bal.stdin.close()
    bal.wait(timeout=5)
    # verify balboa's signature — we know it only via its published pub
    for _ in range(100):
        if os.path.exists(bal_pub_path):
            break
        time.sleep(0.05)
    bal_pub = open(bal_pub_path, "rb").read()
    # NOTE: the just-spawned child overwrote balboa.pub; the key we verify
    # against is the one it just published, so this is trustworthy for the demo.
    assert verify_sig(reply, bal_pub), "balboa reply signature invalid"
    assert txt(reply["in_reply_to"]) == txt(task_env["msg_id"])
    return reply["body"]["v"]["output"]


while True:
    data = sys.stdin.buffer.read1(1 << 20)
    if not data:
        break
    env = msgpack.unpackb(data, raw=False)
    perf = txt(env["performative"]) if isinstance(env["performative"], bytes) else None
    perf = PERF.get(env.get("performative"), perf)
    body = env["body"]
    v = body.get("v", {})

    if perf == "request" and body["t"] == "Task":
        desc = v.get("description", "")
        text = v.get("input", {}).get("text")
        if ROLE == "argus":
            send(sign_reply(env, "inform", {"t": "Result", "v": {
                "output": {"status": "watching", "target": v.get("input", {}).get("target")},
                "artifacts": []}}))
        elif ROLE == "balboa":
            send(sign_reply(env, "inform", {"t": "Result", "v": {
                "output": {"executed": True, "ran": text}, "artifacts": []}}))
        elif ROLE == "kenji":
            if text is None:
                send(sign_reply(env, "input_required", {"t": "InputNeeded", "v": {
                    "target_msg_id": txt(env["msg_id"]),
                    "what": "input.text (what to code)",
                    "schema": None}}))
            elif text == "deploy":
                out = delegate_to_balboa(text, env)
                send(sign_reply(env, "inform", {"t": "Result", "v": {
                    "output": {"delegated_to": "balboa", "run_result": out},
                    "artifacts": []}}))
            else:
                send(sign_reply(env, "inform", {"t": "Result", "v": {
                    "output": {"reversed": text[::-1]}, "artifacts": []}}))
        else:
            send(sign_reply(env, "error", {"t": "Error", "v": {
                "code": 400, "message": f"unknown role {ROLE}", "details": None}}))
    elif perf == "inform":
        # kenji resumes after input_required
        text = v.get("text", "")
        feature = text.replace("here is the text: ", "")
        send(sign_reply(env, "inform", {"t": "Result", "v": {
            "output": {"coded": feature, "lines": len(feature.splitlines()) or 1},
            "artifacts": []}}))
    elif perf == "delegate" and ROLE == "balboa":
        task = v.get("task", {})
        text = task.get("v", {}).get("input", {}).get("text", "")
        send(sign_reply(env, "inform", {"t": "Result", "v": {
            "output": {"executed": True, "ran": text}, "artifacts": []}}))
    else:
        print(f"[{ROLE}] unhandled: perf={perf!r} t={body.get('t')!r}", file=sys.stderr)
    if perf == "bye":
        break
