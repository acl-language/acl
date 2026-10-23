// Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0).
// Exposes one tool: text.reverse. Used to validate the ACL↔MCP bridge
// against a real MCP-speaking peer.
const readline = require("readline");

const tools = [
  {
    name: "text.reverse",
    description: "Reverse a text string",
    inputSchema: { type: "object", required: ["text"], properties: { text: { type: "string" } } },
  },
];

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.id === undefined) return; // notification — ignore
  switch (msg.method) {
    case "initialize":
      send({
        jsonrpc: "2.0", id: msg.id,
        result: {
          protocolVersion: msg.params?.protocolVersion || "2025-06-18",
          capabilities: { tools: {} },
          serverInfo: { name: "text-reverse-mcp", version: "0.1.0" },
        },
      });
      break;
    case "tools/list":
      send({ jsonrpc: "2.0", id: msg.id, result: { tools } });
      break;
    case "tools/call": {
      const text = msg.params?.arguments?.text || "";
      send({
        jsonrpc: "2.0", id: msg.id,
        result: {
          content: [{ type: "text", text: [...text].reverse().join("") }],
          isError: false,
        },
      });
      break;
    }
    default:
      send({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "method not found" } });
  }
});

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}
