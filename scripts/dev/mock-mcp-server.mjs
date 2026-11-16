// Minimal MCP server for manual testing without a real package.
//   stdio:           node scripts/dev/mock-mcp-server.mjs            (use as a stdio command)
//   Streamable HTTP: node scripts/dev/mock-mcp-server.mjs --http 8788
//
// Tools: echo(text), add(a,b), get_time, fail, slow(seconds) — plus `--many N` pads the
// catalog with N no-op tools (filler_0..N) to exercise on-demand tool loading.
//
// OAuth (HTTP mode only): `--oauth` makes /mcp require a bearer token and serves the
// whole flow on the same origin — protected-resource + authorization-server metadata,
// /register (Dynamic Client Registration), /authorize (auto-approves and redirects),
// /token (authorization_code + PKCE S256, refresh_token, client_credentials).
//   --expires-in N    access-token lifetime in seconds (default 3600)
//   --no-dcr          no registration endpoint: sign-in needs a Client ID (any value works)
//   --reject-refresh  answer refreshes with invalid_grant (forces a new sign-in)
// Client credentials: client ID `mock-client`, secret `mock-secret`.
import { createHash, randomBytes } from "node:crypto";
import { createServer } from "node:http";

const args = process.argv.slice(2);
const httpIdx = args.indexOf("--http");
const httpPort = httpIdx >= 0 ? Number(args[httpIdx + 1] ?? 8788) : null;
const manyIdx = args.indexOf("--many");
const fillerCount = manyIdx >= 0 ? Number(args[manyIdx + 1] ?? 0) : 0;
const oauth = args.includes("--oauth");
const noDcr = args.includes("--no-dcr");
const rejectRefresh = args.includes("--reject-refresh");
const expIdx = args.indexOf("--expires-in");
const expiresIn = expIdx >= 0 ? Number(args[expIdx + 1] ?? 3600) : 3600;
const CC_ID = "mock-client";
const CC_SECRET = "mock-secret";

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
  serverInfo: {
    name: "mock-mcp",
    title: "Mock MCP Server",
    version: "0.1.0",
    // 1×1 PNGs (light: blue, dark: orange) to exercise server icons; the https one is ignored.
    icons: [
      { src: "https://example.com/icon.png", mimeType: "image/png" },
      { src: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNQTX4NAAIkAXSaGkHUAAAAAElFTkSuQmCC", mimeType: "image/png", theme: "light" },
      { src: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4WSwGAAPrAYPijIcLAAAAAElFTkSuQmCC", mimeType: "image/png", theme: "dark" },
    ],
  },
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
  // Same origin the client used (localhost or 127.0.0.1) so metadata matches the resource.
  const originOf = (req) => `http://${req.headers.host ?? `127.0.0.1:${httpPort}`}`;
  const accessTokens = new Map(); // token -> expiry (ms)
  const refreshTokens = new Set();
  const codes = new Map(); // code -> { clientId, challenge, redirectUri }
  const token = () => randomBytes(18).toString("base64url");
  const json = (res, status, body, headers = {}) => {
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", ...headers });
    res.end(JSON.stringify(body));
  };
  const readBody = async (req) => {
    let body = "";
    for await (const c of req) body += c;
    return body;
  };
  const issue = (withRefresh) => {
    const at = token();
    accessTokens.set(at, Date.now() + expiresIn * 1000);
    const out = { access_token: at, token_type: "Bearer", expires_in: expiresIn };
    if (withRefresh) {
      const rt = token();
      refreshTokens.add(rt);
      out.refresh_token = rt;
    }
    return out;
  };

  // Returns true when the request was an OAuth endpoint (and has been answered).
  async function handleOAuth(req, res, url) {
    const origin = originOf(req);
    const p = url.pathname;
    if (p.startsWith("/.well-known/oauth-protected-resource")) {
      json(res, 200, { resource: `${origin}/mcp`, authorization_servers: [origin], scopes_supported: ["mcp"] });
      return true;
    }
    if (p.startsWith("/.well-known/oauth-authorization-server")) {
      json(res, 200, {
        issuer: origin,
        authorization_endpoint: `${origin}/authorize`,
        token_endpoint: `${origin}/token`,
        ...(noDcr ? {} : { registration_endpoint: `${origin}/register` }),
        response_types_supported: ["code"],
        grant_types_supported: ["authorization_code", "refresh_token", "client_credentials"],
        code_challenge_methods_supported: ["S256"],
        token_endpoint_auth_methods_supported: ["none", "client_secret_post", "client_secret_basic"],
        scopes_supported: ["mcp"],
      });
      return true;
    }
    if (req.method === "POST" && p === "/register" && !noDcr) {
      const reg = JSON.parse((await readBody(req)) || "{}");
      json(res, 201, { client_id: `dcr-${token()}`, redirect_uris: reg.redirect_uris ?? [] });
      return true;
    }
    if (req.method === "GET" && p === "/authorize") {
      const q = url.searchParams;
      const redirect = q.get("redirect_uri") ?? "";
      if (q.get("code_challenge_method") !== "S256" || !q.get("code_challenge") || !/^http:\/\/127\.0\.0\.1:\d+\//.test(redirect)) {
        json(res, 400, { error: "invalid_request" });
        return true;
      }
      const code = token();
      codes.set(code, { clientId: q.get("client_id"), challenge: q.get("code_challenge"), redirectUri: redirect });
      const loc = new URL(redirect);
      loc.searchParams.set("code", code);
      loc.searchParams.set("state", q.get("state") ?? "");
      console.log(`authorize: approved ${q.get("client_id")}`);
      res.writeHead(302, { location: loc.toString() }).end();
      return true;
    }
    if (req.method === "POST" && p === "/token") {
      const f = new URLSearchParams(await readBody(req));
      const grant = f.get("grant_type");
      let [basicId, basicSecret] = [null, null];
      const auth = req.headers.authorization ?? "";
      if (auth.startsWith("Basic ")) [basicId, basicSecret] = Buffer.from(auth.slice(6), "base64").toString().split(":").map(decodeURIComponent);
      console.log(`token: ${grant}`);
      if (grant === "authorization_code") {
        const c = codes.get(f.get("code") ?? "");
        codes.delete(f.get("code") ?? "");
        const hashed = createHash("sha256").update(f.get("code_verifier") ?? "").digest("base64url");
        const clientId = f.get("client_id") ?? basicId;
        if (c && c.challenge === hashed && c.clientId === clientId && c.redirectUri === f.get("redirect_uri")) return json(res, 200, issue(true)), true;
        return json(res, 400, { error: "invalid_grant" }), true;
      }
      if (grant === "refresh_token") {
        const rt = f.get("refresh_token") ?? "";
        if (!rejectRefresh && refreshTokens.delete(rt)) return json(res, 200, issue(true)), true;
        return json(res, 400, { error: "invalid_grant" }), true;
      }
      if (grant === "client_credentials") {
        const id = f.get("client_id") ?? basicId;
        const secret = f.get("client_secret") ?? basicSecret;
        if (id === CC_ID && secret === CC_SECRET && f.get("resource")) return json(res, 200, issue(false)), true;
        return json(res, 401, { error: "invalid_client" }), true;
      }
      return json(res, 400, { error: "unsupported_grant_type" }), true;
    }
    return false;
  }

  createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", `http://localhost:${httpPort}`);
    if (oauth && (await handleOAuth(req, res, url))) return;
    if (oauth && url.pathname === "/mcp") {
      const bearer = (req.headers.authorization ?? "").replace(/^Bearer /, "");
      const exp = accessTokens.get(bearer);
      if (!exp || exp < Date.now()) {
        res.writeHead(401, { "www-authenticate": `Bearer resource_metadata="${originOf(req)}/.well-known/oauth-protected-resource/mcp"` });
        res.end();
        return;
      }
    }
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
    console.log(
      `mock MCP (Streamable HTTP) on http://localhost:${httpPort}/mcp — ${tools.length} tools` +
        (oauth ? ` · OAuth${noDcr ? " (no DCR)" : ""}, tokens ${expiresIn}s; client credentials ${CC_ID} / ${CC_SECRET}` : ""),
    ),
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
