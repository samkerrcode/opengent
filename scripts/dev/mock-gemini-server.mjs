// Minimal Gemini Interactions API server for manual testing without a Google account.
// Usage: node scripts/dev/mock-gemini-server.mjs [port]
// Then add a "Google Gemini" provider with base URL http://localhost:<port>/v1 (default 8789)
// and any API key except "bad" (which is rejected like an invalid AI Studio key).
//
// - Models: mock-gemini-fast (no thinking), mock-gemini-thinking (streams thought summaries
//   and a signed thought step), mock-gemini-slow.
// - "use tool <name>" against a request with tools returns a signed function_call step and
//   status requires_action; the follow-up must carry a function_result for every call.
// - Replay validation: every thought / function_call signature this server issued must come
//   back unchanged and in place, or the request fails with 400 (catches replay bugs).
import { createServer } from "node:http";
import { createHash } from "node:crypto";

const port = Number(process.argv[2] ?? 8789);
const models = [
  { name: "models/mock-gemini-fast", inputTokenLimit: 1048576, supportedGenerationMethods: ["generateContent"] },
  { name: "models/mock-gemini-thinking", inputTokenLimit: 1048576, supportedGenerationMethods: ["generateContent"], thinking: true },
  { name: "models/mock-gemini-slow", inputTokenLimit: 32768, supportedGenerationMethods: ["generateContent"] },
  { name: "models/mock-embedding-001", supportedGenerationMethods: ["embedContent"] },
];
const SECRET = "mock-gemini";
const sign = (kind, payload) => createHash("sha256").update(`${SECRET}:${kind}:${payload}`).digest("base64");
/** Thought signatures this server issued (process lifetime). */
const issuedThoughts = new Set();

const json = (res, status, body) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const googleError = (res, status, code, message, reason) =>
  json(res, status, { error: { code: status, message, status: code, details: reason ? [{ reason }] : [] } });

/** Validates replayed model steps; returns an error message or null. */
function validateReplay(input) {
  const callIds = new Set();
  for (const step of input) {
    if (step.type === "thought") {
      if (!issuedThoughts.has(step.signature)) return "thought step has an unknown or altered signature";
      // Summary-less replays are accepted (the adapter drops summaries over its size cap).
      if (step.summary) {
        const text = step.summary.map((s) => s.text ?? "").join("");
        if (sign("thought", text) !== step.signature) return "thought summary was altered";
      }
    }
    if (step.type === "function_call") {
      callIds.add(step.id);
      if (step.signature !== sign("call", `${step.id}:${step.name}`)) return `function_call ${step.id} lost or changed its signature`;
    }
    if (step.type === "function_result" && !callIds.has(step.call_id)) return `function_result for unknown call ${step.call_id}`;
  }
  for (const id of callIds) {
    if (!input.some((s) => s.type === "function_result" && s.call_id === id)) return `call ${id} has no function_result`;
  }
  return null;
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://localhost:${port}`);
  const key = req.headers["x-goog-api-key"];
  if (!url.pathname.startsWith("/v1/") && !url.pathname.startsWith("/v1beta/")) {
    return googleError(res, 404, "NOT_FOUND", `no route ${req.method} ${url.pathname}`);
  }
  if (!key || key === "bad") {
    return googleError(res, 400, "INVALID_ARGUMENT", "API key not valid. Please pass a valid API key.", "API_KEY_INVALID");
  }
  const path = url.pathname.replace(/^\/v1(beta)?/, "");
  if (req.method === "GET" && path === "/models") {
    return json(res, 200, { models });
  }
  if (req.method !== "POST" || path !== "/interactions") {
    return googleError(res, 404, "NOT_FOUND", `no route ${req.method} ${url.pathname}`);
  }

  let raw = "";
  for await (const chunk of req) raw += chunk;
  const body = JSON.parse(raw);
  if (body.store !== false) return googleError(res, 400, "INVALID_ARGUMENT", "this mock only supports store:false");
  if (!body.stream) return googleError(res, 400, "INVALID_ARGUMENT", "this mock only supports stream:true");
  const input = Array.isArray(body.input) ? body.input : [];
  const invalid = validateReplay(input);
  if (invalid) return googleError(res, 400, "INVALID_ARGUMENT", `replay check failed: ${invalid}`);

  const model = String(body.model ?? "");
  const delay = model.includes("slow") ? 60 : 12;
  const lastUser = [...input].reverse().find((s) => s.type === "user_input");
  const userText = (lastUser?.content ?? []).filter((p) => p.type === "text").map((p) => p.text).join("\n");
  const images = (lastUser?.content ?? []).filter((p) => p.type === "image").map((p) => p.mime_type);
  const results = input.filter((s) => s.type === "function_result");
  const lastIsResult = input.at(-1)?.type === "function_result";

  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  const send = (event_type, data) => res.write(`event: ${event_type}\ndata: ${JSON.stringify({ ...data, event_type })}\n\n`);
  const sleep = () => new Promise((r) => setTimeout(r, delay));
  let index = 0;
  send("interaction.created", { interaction: { status: "in_progress", model } });

  if (model.includes("thinking") && body.generation_config?.thinking_summaries === "auto") {
    const parts = ["**Considering the request**\n\n", "The user wants a reply; ", "I'll answer concisely."];
    const i = index++;
    send("step.start", { index: i, step: { type: "thought", signature: "", summary: [{ type: "text", text: parts[0] }] } });
    for (const p of parts.slice(1)) {
      await sleep();
      send("step.delta", { index: i, delta: { type: "thought_summary", content: { type: "text", text: p } } });
    }
    const signature = sign("thought", parts.join(""));
    issuedThoughts.add(signature);
    send("step.delta", { index: i, delta: { type: "thought_signature", signature } });
    send("step.stop", { index: i });
  }

  const words = (s) => s.match(/\S+\s*/g) ?? [];
  const streamText = async (text) => {
    const [first, ...rest] = words(text);
    const i = index++;
    send("step.start", { index: i, step: { type: "model_output", content: [{ type: "text", text: first ?? "" }] } });
    for (const w of rest) {
      await sleep();
      send("step.delta", { index: i, delta: { type: "text", text: w } });
    }
    send("step.stop", { index: i });
  };

  const toolCall = !lastIsResult && body.tools?.length && /use tool (\S+)/.exec(userText);
  if (toolCall) {
    const wanted = toolCall[1];
    const name = body.tools.some((t) => t.name === wanted) ? wanted : body.tools[0].name;
    await streamText(`Calling the \`${name}\` tool now.`);
    const id = `call_${Date.now().toString(36)}`;
    const i = index++;
    send("step.start", { index: i, step: { type: "function_call", id, name } });
    const args = JSON.stringify({ text: "hello from the mock Gemini model" });
    send("step.delta", { index: i, delta: { type: "arguments_delta", arguments: args.slice(0, 10) } });
    send("step.delta", { index: i, delta: { type: "arguments_delta", arguments: args.slice(10), signature: sign("call", `${id}:${name}`) } });
    send("step.stop", { index: i });
    send("interaction.completed", { interaction: { status: "requires_action", usage: { total_input_tokens: 50, total_output_tokens: 12 } } });
    return res.end();
  }

  const toolNote = results.length
    ? `\n\nThe tool returned:\n\n> ${results.map((r) => JSON.stringify(r.result).slice(0, 400)).join("\n> ")}`
    : "";
  const imageNote = images.length ? `\n\nI received ${images.length} image(s): ${images.join(", ")}.` : "";
  const replayed = input.filter((s) => s.type === "thought").length;
  const reply =
    `## Mock Gemini reply from \`${model}\`\n\nYou said:\n\n> ${userText}${imageNote}${toolNote}\n\n` +
    `Replayed ${replayed} thought step(s) and ${input.filter((s) => s.type === "function_call").length} call(s) — all signatures valid.`;
  await streamText(reply);
  send("interaction.completed", {
    interaction: { status: "completed", usage: { total_input_tokens: 40, total_output_tokens: words(reply).length, total_thought_tokens: 9 } },
  });
  res.end();
}).listen(port, "127.0.0.1", () =>
  console.log(`mock Gemini server on http://localhost:${port}/v1 (models: ${models.map((m) => m.name.slice(7)).join(", ")}; key "bad" is rejected)`),
);
