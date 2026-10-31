// Minimal MCP server for manual testing without a real package.
//   stdio:           node scripts/dev/mock-mcp-server.mjs            (use as a stdio command)
//   Streamable HTTP: node scripts/dev/mock-mcp-server.mjs --http 8788
//
// Tools: echo(text), add(a,b), get_time, fail, slow(seconds) — plus `--many N` pads the
// catalog with N no-op tools (filler_0..N) to exercise on-demand tool loading.
import { createServer } from "node:http";

const args = process.argv.slice(2);
const httpIdx = args.indexOf("--http");
const httpPort = httpIdx >= 0 ? Number(args[httpIdx + 1] ?? 8788) : null;
const manyIdx = args.indexOf("--many");
const fillerCount = manyIdx >= 0 ? Number(args[manyIdx + 1] ?? 0) : 0;

const tools = [
  {
    name: "echo",
    title: "Echo",
    description: "Echo back the text you pass in.",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    run: (a) => `Echo: ${a.text ?? ""}`,
  },
  {
    name: "add",
    title: "Add numbers",
    description: "Add two numbers and return the sum.",
    inputSchema: {
      type: "object",
      properties: { a: { type: "number" }, b: { type: "number" } },
      required: ["a", "b"],
    },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    run: (a) => `${a.a} + ${a.b} = ${Number(a.a) + Number(a.b)}`,
  },
  {
    name: "get_time",
    title: "Current time",
    description: "Return the current date and time.",
    inputSchema: { type: "object", properties: {} },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    run: () => new Date().toISOString(),
  },
  {
    name: "fail",
    title: "Always fails",
    description: "Returns an error result; useful to test failure handling.",
    inputSchema: { type: "object", properties: {} },
    run: () => {
      throw new Error("this tool is designed to fail");
    },
  },
  {
    name: "slow",
    title: "Slow call",
    description: "Sleep for `seconds` then answer; useful to test timeouts and cancel.",
    inputSchema: { type: "object", properties: { seconds: { type: "number" } } },
    run: async (a) => {
      await new Promise((r) => setTimeout(r, Math.min(Number(a.seconds) || 5, 30) * 1000));
      return "done waiting";
    },
  },
];
for (let i = 0; i < fillerCount; i++) {
  tools.push({
    name: `filler_${i}`,
    description: `Filler tool ${i} for discovery testing — describes filler_${i} precisely.`,
    inputSchema: { type: "object", properties: { x: { type: "string" } } },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    run: () => `filler_${i} ran`,
  });
}
const toolList = tools.map(({ run, ...t }) => t);
const byName = new Map(tools.map((t) => [t.name, t]));

const INFO = {
  protocolVersion: "2025-06-18",
  capabilities: { tools: { listChanged: false } },
  serverInfo: { name: "mock-mcp", version: "0.1.0" },
  instructions: "Mock tools for OpenGent development: echo, add, get_time, fail, slow.",
};

async function handle(msg) {
  if (!msg.method) return null;
  switch (msg.method) {
    case "initialize":
      return { result: INFO };
    case "ping":
      return { result: {} };
    case "notifications/initialized":
    case "notifications/cancelled":
      return null; // notification: no response
    case "tools/list":
      return { result: { tools: toolList } };
    case "tools/call": {
      const { name, arguments: a } = msg.params ?? {};
      const tool = byName.get(name);
      if (!tool) return { error: { code: -32602, message: `unknown tool ${name}` } };
      try {
        const text = String(await tool.run(a ?? {}));
        return { result: { content: [{ type: "text", text }] } };
      } catch (e) {
        return { result: { content: [{ type: "text", text: String(e.message ?? e) }], isError: true } };
      }
    }
    default:
      return { error: { code: -32601, message: `method not supported: ${msg.method}` } };
  }
}

function respond(msg, out) {
  if (!("id" in msg)) return null;
  return JSON.stringify({ jsonrpc: "2.0", id: msg.id, ...out });
}

if (httpPort) {
  // Streamable HTTP: POST JSON-RPC → JSON responses, GET → SSE stream endpoint (405-friendly
  // fallback so simple clients still work).
  const sessions = new Set();
  createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", `http://localhost:${httpPort}`);
    if (req.method === "POST" && url.pathname === "/mcp") {
      let body = "";
      for await (const c of req) body += c;
      const session = req.headers["mcp-session-id"] ?? `s-${Math.random().toString(36).slice(2)}`;
      const batch = JSON.parse(body);
      const msgs = Array.isArray(batch) ? batch : [batch];
      const replies = [];
      for (const m of msgs) {
        const out = await handle(m);
        const r = respond(m, out ?? {});
        if (r) replies.push(JSON.parse(r));
      }
      res.writeHead(200, { "content-type": "application/json", "mcp-session-id": session });
      res.end(JSON.stringify(Array.isArray(batch) ? replies : replies[0] ?? {}));
      sessions.add(session);
      return;
    }
    if (req.method === "GET" && url.pathname === "/mcp") {
      res.writeHead(405, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "GET streams not implemented" }));
      return;
    }
    res.writeHead(404).end();
  }).listen(httpPort, "127.0.0.1", () =>
    console.log(`mock MCP (Streamable HTTP) on http://localhost:${httpPort}/mcp — ${tools.length} tools`),
  );
} else {
  // stdio: one JSON-RPC message per line.
  process.stdin.setEncoding("utf8");
  let buf = "";
  process.stdin.on("data", async (chunk) => {
    buf += chunk;
    for (;;) {
      const i = buf.indexOf("\n");
      if (i < 0) break;
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      const out = await handle(msg);
      const r = respond(msg, out ?? {});
      if (r) process.stdout.write(r + "\n");
    }
  });
  process.stderr.write(`mock MCP (stdio) ready — ${tools.length} tools\n`);
}
