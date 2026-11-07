// Minimal Anthropic Messages API server for manual testing without an Anthropic account.
// Usage: node scripts/dev/mock-anthropic-server.mjs [port]
// Then add an "Anthropic" provider with base URL http://localhost:<port>/v1 (default 8790)
// and any API key except "bad" (which is rejected like an invalid Anthropic key).
//
// - Models: mock-claude-fast (no thinking config needed), mock-claude-thinking (streams a
//   signed thinking block), mock-claude-slow.
// - "use tool <name>" against a request with tools returns a tool_use block and
//   stop_reason tool_use; the follow-up must carry a tool_result for every call.
// - "decline" returns a classifier-style refusal stop.
// - Replay validation: every thinking signature this server issued must come back
//   unchanged (a `thinking` field other than "" or the original text is an alteration),
//   or the request fails with 400 (catches replay bugs).
import { createServer } from "node:http";
import { createHash } from "node:crypto";

const port = Number(process.argv[2] ?? 8790);
const MODEL_INFO = {
  "mock-claude-fast": { thinking: false },
  "mock-claude-thinking": { thinking: true },
  "mock-claude-slow": { thinking: true, slow: true },
};
const models = Object.keys(MODEL_INFO).map((id) => ({
  type: "model",
  id,
  display_name: id,
  created_at: "2026-06-01T00:00:00Z",
  lifecycle: "active",
  line: "sonnet",
  max_input_tokens: 1000000,
  max_tokens: 128000,
  capabilities: {
    batch: { supported: true },
    image_input: { supported: true },
    thinking: {
      supported: MODEL_INFO[id].thinking,
      types: {
        adaptive: { supported: MODEL_INFO[id].thinking },
        between_tools: { supported: false },
        disabled: { supported: !MODEL_INFO[id].thinking },
        enabled: { supported: false },
      },
    },
    server_tools: { supported: false },
  },
}));
const SECRET = "mock-anthropic";
const sign = (payload) => `sig_${createHash("sha256").update(`${SECRET}:${payload}`).digest("hex").slice(0, 16)}`;
/** Thinking signatures this server issued (process lifetime). */
const issuedThoughts = new Map(); // signature -> thinking text it was issued with

const json = (res, status, body) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const apiError = (res, status, type, message) =>
  json(res, status, { type: "error", error: { type, message } });

/** Validates replayed thinking/tool_use blocks; returns an error message or null. */
function validateReplay(messages) {
  const callIds = new Set();
  for (const m of messages) {
    for (const b of m.content ?? []) {
      if (b.type === "thinking" || b.type === "redacted_thinking") {
        const original = issuedThoughts.get(b.signature ?? b.data);
        if (original === undefined) {
          return `messages: Invalid signature in ${b.type} block — unknown or altered`;
        }
        // "" is what display:"omitted" produces and is always accepted; anything else
        // must be the text the signature was issued with.
        if (b.type === "thinking" && b.thinking !== "" && b.thinking !== original) {
          return "thinking block text was altered";
        }
      }
      if (b.type === "tool_use") callIds.add(b.id);
      if (b.type === "tool_result" && !callIds.has(b.tool_use_id)) {
        return `tool_result for unknown call ${b.tool_use_id}`;
      }
    }
  }
  for (const id of callIds) {
    const answered = messages.some((m) =>
      (m.content ?? []).some((b) => b.type === "tool_result" && b.tool_use_id === id),
    );
    if (!answered) return `call ${id} has no tool_result`;
  }
  return null;
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://localhost:${port}`);
  if (!url.pathname.startsWith("/v1/")) {
    return apiError(res, 404, "not_found_error", `no route ${req.method} ${url.pathname}`);
  }
  const key = req.headers["x-api-key"];
  if (!key || key === "bad") {
    return apiError(res, 401, "authentication_error", "invalid x-api-key");
  }
  const path = url.pathname.replace(/^\/v1/, "");
  if (req.method === "GET" && path === "/models") {
    return json(res, 200, { data: models, has_more: false, first_id: models[0].id, last_id: models.at(-1).id });
  }
  if (req.method !== "POST" || path !== "/messages") {
    return apiError(res, 404, "not_found_error", `no route ${req.method} ${url.pathname}`);
  }

  let raw = "";
  for await (const chunk of req) raw += chunk;
  const body = JSON.parse(raw);
  if (!body.stream) return apiError(res, 400, "invalid_request_error", "this mock only supports stream:true");
  for (const p of ["temperature", "top_p", "top_k"]) {
    if (p in body) return apiError(res, 400, "invalid_request_error", `${p} is not supported in this API version`);
  }
  if (!("max_tokens" in body)) return apiError(res, 400, "invalid_request_error", "max_tokens: Field required");
  if (body.max_tokens > 128000) return apiError(res, 400, "invalid_request_error", "max_tokens exceeds the model's 128000 limit");
  if (body.thinking?.type && !["adaptive", "disabled", "between_tools"].includes(body.thinking.type)) {
    return apiError(res, 400, "invalid_request_error", `thinking.type '${body.thinking.type}' is not supported by this model`);
  }
  const messages = Array.isArray(body.messages) ? body.messages : [];
  const invalid = validateReplay(messages);
  if (invalid) return apiError(res, 400, "invalid_request_error", `replay check failed: ${invalid}`);

  const model = String(body.model ?? "");
  const delay = MODEL_INFO[model]?.slow ? 60 : 12;
  const lastUser = [...messages].reverse().find((m) => m.role === "user");
  const userText = (lastUser?.content ?? [])
    .filter((p) => p.type === "text" || p.type === "tool_result")
    .map((p) => (p.type === "text" ? p.text : ""))
    .join("\n");
  const lastIsResult = lastUser?.content?.at(-1)?.type === "tool_result";

  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  const send = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
  const sleep = () => new Promise((r) => setTimeout(r, delay));
  let index = 0;
  send("message_start", {
    message: {
      id: `msg_${Date.now().toString(36)}`,
      type: "message",
      role: "assistant",
      model,
      content: [],
      stop_reason: null,
      usage: { input_tokens: 40, cache_creation_input_tokens: 0, cache_read_input_tokens: 0, output_tokens: 1 },
    },
  });

  const words = (s) => s.match(/\S+\s*/g) ?? [];
  const streamText = async (text) => {
    const [first, ...rest] = words(text);
    const i = index++;
    send("content_block_start", { index: i, content_block: { type: "text", text: first ?? "" } });
    for (const w of rest) {
      await sleep();
      send("content_block_delta", { index: i, delta: { type: "text_delta", text: w } });
    }
    send("content_block_stop", { index: i });
  };
  const finish = (stop_reason, extra = {}) => {
    send("message_delta", { delta: { stop_reason, stop_sequence: null, ...extra }, usage: { output_tokens: 30 } });
    send("message_stop", {});
    res.end();
  };

  if (body.thinking?.type === "adaptive" && MODEL_INFO[model]?.thinking) {
    const parts = ["Checking what the request needs; ", "a short answer will do."];
    const i = index++;
    send("content_block_start", { index: i, content_block: { type: "thinking", thinking: "", signature: "" } });
    for (const p of parts) {
      await sleep();
      send("content_block_delta", { index: i, delta: { type: "thinking_delta", thinking: p } });
    }
    const signature = sign(parts.join(""));
    issuedThoughts.set(signature, parts.join(""));
    send("content_block_delta", { index: i, delta: { type: "signature_delta", signature } });
    send("content_block_stop", { index: i });
  }

  if (/decline/.test(userText)) {
    return finish("refusal", {
      stop_details: { type: "refusal", category: "cyber", explanation: "This request was declined by the mock classifier." },
    });
  }

  const toolCall = !lastIsResult && body.tools?.length && /use tool (\S+)/.exec(userText);
  if (toolCall) {
    const wanted = toolCall[1];
    const name = body.tools.some((t) => t.name === wanted) ? wanted : body.tools[0].name;
    await streamText(`Calling the \`${name}\` tool now.`);
    const i = index++;
    send("content_block_start", { index: i, content_block: { type: "tool_use", id: `toolu_${Date.now().toString(36)}`, name, input: {} } });
    const args = JSON.stringify({ text: "hello from the mock Anthropic model" });
    send("content_block_delta", { index: i, delta: { type: "input_json_delta", partial_json: args.slice(0, 12) } });
    send("content_block_delta", { index: i, delta: { type: "input_json_delta", partial_json: args.slice(12) } });
    send("content_block_stop", { index: i });
    return finish("tool_use");
  }

  const results = messages.flatMap((m) => (m.content ?? []).filter((b) => b.type === "tool_result"));
  const toolNote = results.length
    ? `\n\nThe tool returned:\n\n> ${results.map((r) => JSON.stringify(r.content).slice(0, 300)).join("\n> ")}`
    : "";
  const replayed = messages.flatMap((m) => m.content ?? []).filter((b) => b.type === "thinking").length;
  const reply =
    `## Mock Anthropic reply from \`${model}\`\n\nYou said:\n\n> ${userText}${toolNote}\n\n` +
    `Replayed ${replayed} thinking block(s) — all signatures valid.`;
  await streamText(reply);
  return finish("end_turn");
}).listen(port, () => {
  console.log(`mock Anthropic Messages API on http://localhost:${port}/v1 (models: ${models.map((m) => m.id).join(", ")}; key "bad" → 401)`);
});
