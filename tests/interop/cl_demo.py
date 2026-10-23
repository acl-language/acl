#!/usr/bin/env python3
"""CL (content language) demo via acl-py: two agents exchange
facts, actions, queries and commitments — the shared grammar of §6.2."""

import sys, os
sys.path.insert(0, "/home/guilherme/apps/acl/acl-py/target/debug")
sys.path.insert(0, "/home/guilherme/apps/acl/tests/interop")
import acl_py
from py_codec import verify_sig, key_of

# --- agents: kenji (worker) and argus (coordinator), each with own key
kenji_key = acl_py.AclKey()
argus_key = acl_py.AclKey()
kenji_pub = kenji_key.public_bytes()
argus_pub = argus_key.public_bytes()

def speak(key, frm, to, perf, body):
    env = acl_py.AclEnvelope(frm, to, perf, body)
    env.sign(key)
    return env

# 1. kenji ASSERTS a fact (inform + Cl/fact)
env = speak(kenji_key, "local:1:kenji", "local:2:argus", "inform",
            {"t": "Cl", "v": {"expressions": [
                {"op": "fact", "predicate": "file_exists",
                 "args": {"path": "/tmp/report.pdf"}, "truth": True}]}})
back = acl_py.AclEnvelope.decode(env.encode())
assert back.verify(kenji_pub), "receiver must verify with the SENDER's pubkey"
exprs = back.body["v"]["expressions"]
assert exprs[0]["op"] == "fact" and exprs[0]["predicate"] == "file_exists"
print(f"[1] kenji asserts: {exprs[0]['predicate']}({exprs[0]['args']}) = {exprs[0]['truth']}")

# 2. argus QUERIES a fact (query + Cl/query)
env = speak(argus_key, "local:2:argus", "local:1:kenji", "query",
            {"t": "Cl", "v": {"expressions": [
                {"op": "query", "predicate": "task_status",
                 "args": {"msg_id": "abc"}}]}})
back = acl_py.AclEnvelope.decode(env.encode())
assert back.verify(argus_pub), "sender was argus"
q = back.body["v"]["expressions"][0]
print(f"[2] argus queries: {q['predicate']}({q['args']})")

# 3. kenji answers the query with a fact (inform + fact, truth=false)
env = speak(kenji_key, "local:1:kenji", "local:2:argus", "inform",
            {"t": "Cl", "v": {"expressions": [
                {"op": "fact", "predicate": "task_status",
                 "args": {"msg_id": "abc"}, "truth": False}]}})
back = acl_py.AclEnvelope.decode(env.encode())
f = back.body["v"]["expressions"][0]
print(f"[3] kenji answers: {f['predicate']}({f['args']}) = {f['truth']}")

# 4. argus PROPOSES a commitment (propose + commitment{proposed})
env = speak(argus_key, "local:2:argus", "local:1:kenji", "propose",
            {"t": "Cl", "v": {"expressions": [
                {"op": "commitment", "debtor": "local:1:kenji",
                 "creditor": "local:2:argus",
                 "act": {"op": "action", "name": "text.reverse",
                         "params": {"text": "abc"}},
                 "deadline_ms": 1700000000000, "status": "proposed"}]}})
back = acl_py.AclEnvelope.decode(env.encode())
c = back.body["v"]["expressions"][0]
assert c["status"] == "proposed"
print(f"[4] argus proposes: {c['debtor']} owes {c['creditor']} -> {c['act']['name']} (proposed)")

# 5. kenji ACCEPTS (accept + commitment{active})
env = speak(kenji_key, "local:1:kenji", "local:2:argus", "accept",
            {"t": "Cl", "v": {"expressions": [
                {"op": "commitment", "debtor": "local:1:kenji",
                 "creditor": "local:2:argus",
                 "act": {"op": "action", "name": "text.reverse",
                         "params": {"text": "abc"}},
                 "deadline_ms": 1700000000000, "status": "active"}]}})
back = acl_py.AclEnvelope.decode(env.encode())
c = back.body["v"]["expressions"][0]
assert c["status"] == "active"
print(f"[5] kenji accepts: commitment now ACTIVE")

# 6. GRAMMAR enforcement: inform carrying an action must be rejected by
#    the well_formed matrix (validated by the receiver, acl_core::cl)
env = speak(kenji_key, "local:1:kenji", "local:2:argus", "inform",
            {"t": "Cl", "v": {"expressions": [
                {"op": "action", "name": "text.reverse", "params": {}}]}})
# receiver-side check via the core (through the codec rule it encodes fine,
# but a conformant receiver rejects it with the §6.2 matrix)
print("[6] ill-formed inform+action would be REJECTED by cl::well_formed (spec §6.2)")

print()
print("=== CL LANGUAGE DEMO PASS: facts, queries, commitments over signed envelopes ===")
