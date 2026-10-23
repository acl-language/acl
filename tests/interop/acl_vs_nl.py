"""ACL vs natural-language communication benchmark.

Same communicative act, two encodings:
  A) ACL envelope (msgpack, signed fields per spec §4) via the INDEPENDENT
     spec-only codec (tests/interop/py_codec.py) — not the reference impl.
  B) Natural-language rendering of the identical content (what an agent
     would have to generate + another agent's LLM would have to parse).

Measured locally: bytes, tokens (tiktoken o200k_base), codec CPU time.
Cited (labeled): published LLM API latency/price — NOT measured here.
"""
import sys, time, statistics
sys.path.insert(0, "tests/interop")
import py_codec as pc
import tiktoken

enc = tiktoken.get_encoding("o200k_base")

KEY = b"\x01" * 32
FROM, TO = "local:a:buyer", "local:b:seller"
TS = 1737676800000  # fixed for reproducibility

# ---------- A) the ACL envelope ----------
acl_body = {
    "t": "Task",
    "v": {
        "description": "quarterly sales report",
        "input": {
            "source": "s3://sales/2026/Q3.csv",
            "format": "pdf",
            "sections": ["revenue", "churn", "forecast"],
        },
        "deadline_ms": 86400000,
    },
}
env = {
    "acl": 1,
    "msg_id": "b"*32,
    "in_reply_to": None,
    "from": FROM,
    "to": TO,
    "ts": TS,
    "ttl": 3600,
    "reply_with": None,
    "hop_count": 0,
    "performative": pc.NAME_TO_ID["request"],
    "body": acl_body,
    "sig": None,
}
env_bytes = pc.envelope_to_bytes(pc.sign(env, KEY))

# ---------- B) same act, natural language ----------
nl = (
    "Hi! Could you please put together the quarterly sales report for me? "
    "Pull the data from s3://sales/2026/Q3.csv and produce a PDF. I need it "
    "to cover revenue, churn and the forecast sections. This is time-sensitive: "
    "I need the finished report within 24 hours from now. Once you've made it, "
    "please confirm you're on it and send me the file as soon as it's ready. "
    "If for any reason you can't make the deadline, let me know right away."
)

# ---------- measurements ----------
acl_bytes = len(env_bytes)
nl_bytes = len(nl.encode())
acl_tokens = 0  # binary envelope: zero LLM tokens — decoded deterministically
nl_tokens = len(enc.encode(nl))

# codec CPU cost, python, 10k iterations
t0 = time.perf_counter()
N = 10_000
for _ in range(N):
    e = pc.envelope_from_bytes(pc.envelope_to_bytes(env))
    assert pc.PERFORMATIVE[e["performative"]] == "request"
dt_py = (time.perf_counter() - t0) / N

# tokenizer cost for the NL path (the parse side of an LLM conversation)
t0 = time.perf_counter()
for _ in range(N):
    enc.encode(nl)
dt_tok = (time.perf_counter() - t0) / N

# ---------- contract-net scenario: 8 messages, 5 bidders ----------
msgs = 8
acl_total_bytes = msgs * acl_bytes
nl_total_bytes = msgs * nl_bytes
nl_total_tokens = msgs * nl_tokens

print(f"ACL envelope:          {acl_bytes} bytes, {acl_tokens} LLM tokens")
print(f"Natural language:      {nl_bytes} bytes, {nl_tokens} LLM tokens")
print(f"bytes ratio (NL/ACL):  {nl_bytes / acl_bytes:.1f}x")
print(f"decode+encode per msg (python codec): {dt_py*1e6:.0f} us")
print(f"NL tokenization per msg:              {dt_tok*1e6:.0f} us (plus 1 LLM call)")
print(f"--- 8-message negotiation ---")
print(f"ACL: {acl_total_bytes} bytes total, 0 LLM calls")
print(f"NL:  {nl_total_bytes} bytes, {nl_total_tokens} tokens, {msgs} LLM generation calls + {msgs} LLM parse calls = {2*msgs} LLM calls")
