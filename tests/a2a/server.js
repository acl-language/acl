// Minimal but real A2A server (subset of A2A v1.0 semantics):
//   GET /.well-known/agent-card.json  → Agent Card
//   POST / (JSON-RPC message/send)    → Task with lifecycle states
// Used to validate acl-a2a-bridge against a network A2A-speaking peer.
const http = require("http");

const AGENT_CARD = {
  protocolVersion: "1.0",
  name: "a2a-echo",
  description: "Minimal A2A echo server for bridge validation",
  url: "http://127.0.0.1:9400/",
  capabilities: { streaming: false },
  skills: [{ id: "text.reverse", name: "text.reverse", description: "Reverse text" }],
};

let taskCounter = 0;

const server = http.createServer((req, res) => {
  if (req.method === "GET" && req.url === "/.well-known/agent-card.json") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify(AGENT_CARD));
    return;
  }
  if (req.method === "POST" && req.url === "/") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      let msg;
      try { msg = JSON.parse(body); } catch { res.writeHead(400); res.end("bad json"); return; }
      if (msg.method !== "message/send") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "method not found" } }));
        return;
      }
      // message/send params: { message: { role, parts: [{kind:"text", text}] } }
      const parts = msg.params?.message?.parts || [];
      const text = parts.find((p) => p.kind === "text")?.text || "";
      const reversed = [...text].reverse().join("");
      const task = {
        id: `task-${++taskCounter}`,
        contextId: msg.params?.message?.taskId || `ctx-${taskCounter}`,
        status: { state: "COMPLETED" },
        artifacts: [{ artifactId: "result", parts: [{ kind: "data", data: { reversed } }] }],
      };
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: task }));
    });
    return;
  }
  res.writeHead(404);
  res.end("not found");
});

server.listen(9400, "127.0.0.1", () => console.log("a2a-echo listening on 9400"));
