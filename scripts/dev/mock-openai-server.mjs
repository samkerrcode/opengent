// Minimal OpenAI server for manual testing without a real model. Speaks both protocols:
// Chat Completions (`POST /v1/chat/completions`, for the "OpenAI-compatible" type) and the
// Responses API (`POST /v1/responses`, for the native "OpenAI" type).
// Usage: node scripts/dev/mock-openai-server.mjs [port]
// Then add a provider with base URL http://localhost:<port>/v1 (default 8787). API key "bad"
// is rejected with a 401.
//
// Responses API specifics:
// - Models: mock-fast, mock-slow, mock-vision, and mock-reasoning (streams a reasoning
//   summary and returns an encrypted reasoning item). The listing also carries a
//   non-chat model (text-embedding-mock) to exercise discovery filtering.
// - "use tool <name>" against a request with tools returns a function_call item; the
//   follow-up must answer every call with a function_call_output.
// - Replay validation: requests must use store:false, carry no item ids, replay every
//   reasoning item this server issued unchanged, and declare tools with strict:false
//   unless their schema is strict-compatible — otherwise the request fails with 400.
import { createServer } from "node:http";
import { createHash } from "node:crypto";

const port = Number(process.argv[2] ?? 8787);
const models = ["mock-fast", "mock-slow", "mock-vision", "mock-reasoning"];
const SECRET = "mock-openai";
const sign = (payload) => createHash("sha256").update(`${SECRET}:${payload}`).digest("base64");
/** Encrypted reasoning blobs this server issued (process lifetime). */
const issuedReasoning = new Set();

const sendJson = (res, status, body) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const apiError = (res, status, message, type = "invalid_request_error", code = null) =>
  sendJson(res, status, { error: { message, type, param: null, code } });

/** Validates a Responses request the way the real API would reject it; returns a message or null. */
function validateResponsesRequest(body) {
  if (body.store !== false) return "this mock only supports store:false";
  if (body.stream !== true) return "this mock only supports stream:true";
  for (const t of body.tools ?? []) {
    if (t.type !== "function" || !t.name) return "tools must be function tools with a name";
    if (t.strict !== false && t.parameters?.additionalProperties !== false) {
      return `Invalid schema for function '${t.name}': In context=(), 'additionalProperties' is required to be supplied and to be false.`;
    }
  }
  const input = Array.isArray(body.input) ? body.input : [];
  const calls = new Set();
  for (const item of input) {
    if ("id" in item) return `Item with id '${item.id}' not found. Items are not persisted when \`store\` is set to false.`;
    if (item.type === "reasoning") {
      if (!issuedReasoning.has(item.encrypted_content)) return "The encrypted content for a reasoning item could not be verified.";
      const text = (item.summary ?? []).map((s) => s.text).join("");
      if (item.summary?.length && sign(`reasoning:${text}`) !== item.encrypted_content) return "reasoning summary was altered";
    }
    if (item.type === "function_call") calls.add(item.call_id);
    if (item.type === "function_call_output" && !calls.has(item.call_id)) return `No tool call found for function call output with call_id ${item.call_id}.`;
  }
  for (const id of calls) {
    if (!input.some((i) => i.type === "function_call_output" && i.call_id === id)) return `No tool output found for function call ${id}.`;
  }
  return null;
}

async function handleResponses(req, res, body) {
  const invalid = validateResponsesRequest(body);
  if (invalid) return apiError(res, 400, invalid);
  const input = Array.isArray(body.input) ? body.input : [];
  const model = String(body.model ?? "");
  const delay = model.includes("slow") ? 60 : 12;
  const lastUser = [...input].reverse().find((i) => i.type === "message" && i.role === "user");
  const parts = Array.isArray(lastUser?.content) ? lastUser.content : [{ type: "input_text", text: String(lastUser?.content ?? "") }];
  const userText = parts.filter((p) => p.type === "input_text").map((p) => p.text).join("\n");
  const images = parts.filter((p) => p.type === "input_image").map((p) => /^data:([^;]+);/.exec(p.image_url ?? "")?.[1] ?? "image/?");
  const results = input.filter((i) => i.type === "function_call_output");
  const lastIsResult = input.at(-1)?.type === "function_call_output";

  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  let seq = 0;
  const send = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, sequence_number: seq++, ...data })}\n\n`);
  const sleep = () => new Promise((r) => setTimeout(r, delay));
  const words = (s) => s.match(/\S+\s*/g) ?? [];
  let index = 0;
  send("response.created", { response: { id: "resp_mock", status: "in_progress", model } });

  if (body.reasoning?.summary) {
    const i = index++;
    const summary = ["**Considering the request**\n\nThe user wants a reply; ", "I'll answer concisely."];
    send("response.output_item.added", { output_index: i, item: { id: `rs_${i}`, type: "reasoning", summary: [] } });
    send("response.reasoning_summary_part.added", { output_index: i, item_id: `rs_${i}`, summary_index: 0, part: { type: "summary_text", text: "" } });
    for (const p of summary) {
      await sleep();
      send("response.reasoning_summary_text.delta", { output_index: i, item_id: `rs_${i}`, summary_index: 0, delta: p });
    }
    const text = summary.join("");
    const encrypted_content = body.include?.includes("reasoning.encrypted_content") ? sign(`reasoning:${text}`) : undefined;
    if (encrypted_content) issuedReasoning.add(encrypted_content);
    send("response.output_item.done", {
      output_index: i,
      item: { id: `rs_${i}`, type: "reasoning", status: "completed", summary: [{ type: "summary_text", text }], encrypted_content },
    });
  }

  const streamText = async (text, phase) => {
    const i = index++;
    send("response.output_item.added", { output_index: i, item: { id: `msg_${i}`, type: "message", role: "assistant", status: "in_progress", content: [] } });
    for (const w of words(text)) {
      await sleep();
      send("response.output_text.delta", { output_index: i, item_id: `msg_${i}`, content_index: 0, delta: w });
    }
    send("response.output_text.done", { output_index: i, item_id: `msg_${i}`, content_index: 0, text });
    send("response.output_item.done", {
      output_index: i,
      item: { id: `msg_${i}`, type: "message", role: "assistant", status: "completed", phase, content: [{ type: "output_text", text, annotations: [] }] },
    });
  };
  const usage = (output) => ({ input_tokens: 40, input_tokens_details: { cached_tokens: 0 }, output_tokens: output, output_tokens_details: { reasoning_tokens: body.reasoning ? 9 : 0 }, total_tokens: 40 + output });

  const toolCall = !lastIsResult && body.tools?.length && /use tool (\S+)/.exec(userText);
  if (toolCall) {
    const wanted = toolCall[1];
    const name = body.tools.some((t) => t.name === wanted) ? wanted : body.tools[0].name;
    await streamText(`Calling the \`${name}\` tool now.`, "commentary");
    const i = index++;
    const call_id = `call_${Date.now().toString(36)}`;
    const args = JSON.stringify({ text: "hello from the mock OpenAI model" });
    send("response.output_item.added", { output_index: i, item: { id: `fc_${i}`, type: "function_call", call_id, name, arguments: "", status: "in_progress" } });
    send("response.function_call_arguments.delta", { output_index: i, item_id: `fc_${i}`, delta: args.slice(0, 10) });
    send("response.function_call_arguments.delta", { output_index: i, item_id: `fc_${i}`, delta: args.slice(10) });
    send("response.function_call_arguments.done", { output_index: i, item_id: `fc_${i}`, arguments: args });
    send("response.output_item.done", { output_index: i, item: { id: `fc_${i}`, type: "function_call", call_id, name, arguments: args, status: "completed" } });
    send("response.completed", { response: { id: "resp_mock", status: "completed", usage: usage(12) } });
    return res.end();
  }

  const toolNote = results.length ? `\n\nThe tool returned:\n\n> ${results.map((r) => String(r.output).slice(0, 400)).join("\n> ")}` : "";
  const imageNote = images.length ? `\n\nI received ${images.length} image(s): ${images.join(", ")}.` : "";
  const replayed = input.filter((i) => i.type === "reasoning").length;
  const reply =
    `## Mock Responses reply from \`${model}\`\n\nYou said:\n\n> ${userText}${imageNote}${toolNote}\n\n` +
    `Replayed ${replayed} reasoning item(s) and ${input.filter((i) => i.type === "function_call").length} call(s) — all valid.`;
  await streamText(reply, "final_answer");
  send("response.completed", { response: { id: "resp_mock", status: "completed", usage: usage(words(reply).length) } });
  res.end();
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://localhost:${port}`);
  if (req.method === "GET" && url.pathname === "/v1/models") {
    if (req.headers.authorization === "Bearer bad") return apiError(res, 401, "Incorrect API key provided", "invalid_request_error", "invalid_api_key");
    // Sorts after the chat models, so "Save & test" never picks it as a default.
    const data = [...models, "text-embedding-mock"].map((id) => ({ id, object: "model", owned_by: "mock" }));
    return sendJson(res, 200, { object: "list", data });
  }
  if (req.method === "POST" && url.pathname === "/v1/responses") {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    if (req.headers.authorization === "Bearer bad") return apiError(res, 401, "Incorrect API key provided", "invalid_request_error", "invalid_api_key");
    return handleResponses(req, res, JSON.parse(raw));
  }
  if (req.method === "POST" && url.pathname === "/v1/chat/completions") {
    let body = "";
    for await (const chunk of req) body += chunk;
    const parsed = JSON.parse(body);
    if (req.headers.authorization === "Bearer bad") {
      res.writeHead(401, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "Incorrect API key provided", type: "invalid_request_error" } }));
      return;
    }
    const content = [...parsed.messages].reverse().find((m) => m.role === "user")?.content ?? "";
    // Multimodal requests send content as a parts array; plain ones send a string.
    const parts = Array.isArray(content) ? content : [{ type: "text", text: content }];
    const last = parts.filter((p) => p.type === "text").map((p) => p.text).join("\n");
    const images = parts
      .filter((p) => p.type === "image_url")
      .map((p) => /^data:([^;]+);base64,/.exec(p.image_url?.url ?? "")?.[1] ?? "image/?");
    const imageNote = images.length ? `\n\nI received ${images.length} image(s): ${images.join(", ")}.` : "";
    const delay = parsed.model === "mock-slow" ? 60 : 12;
    const words = (s) => s.match(/\S+\s*/g) ?? [];
    const streamWords = async (reply) => {
      for (const w of words(reply)) {
        res.write(`data: ${JSON.stringify({ id: "x", choices: [{ index: 0, delta: { content: w }, finish_reason: null }] })}\n\n`);
        await new Promise((r) => setTimeout(r, delay));
      }
    };

    // Tool-call trigger: the message "use tool <exposed_name>" against a request that lists
    // tools produces a real tool_calls turn. The follow-up request (which contains role
    // "tool" messages) gets a normal reply summarizing the tool output.
    const toolCall = parsed.tools?.length && /use tool (\S+)/.exec(last);
    if (toolCall) {
      const wanted = toolCall[1];
      const spec = parsed.tools.find((t) => t.function?.name === wanted);
      const name = spec ? wanted : parsed.tools[0].function.name;
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      await streamWords(`Calling the \`${name}\` tool now.`);
      const argsJson = JSON.stringify({ text: "hello from the mock model" });
      res.write(
        `data: ${JSON.stringify({
          id: "x",
          choices: [{
            index: 0,
            delta: { tool_calls: [{ index: 0, id: "call_mock_1", type: "function", function: { name, arguments: argsJson } }] },
            finish_reason: "tool_calls",
          }],
        })}\n\n`,
      );
      res.write(`data: ${JSON.stringify({ id: "x", choices: [], usage: { prompt_tokens: 50, completion_tokens: 12 } })}\n\n`);
      res.write("data: [DONE]\n\n");
      res.end();
      return;
    }

    const toolResults = parsed.messages.filter((m) => m.role === "tool");
    const toolNote = toolResults.length
      ? `\n\nThe tool returned:\n\n> ${toolResults.map((t) => String(t.content).slice(0, 400)).join("\n> ")}`
      : "";
    const reply =
      `## Mock reply from \`${parsed.model}\`\n\nYou said:\n\n> ${last}${imageNote}${toolNote}\n\n` +
      "Here is a code block:\n\n```ts\nconst answer = 42;\n```\n\n" +
      "And a [link](https://example.com). *Streaming* **works**.";
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    await streamWords(reply);
    res.write(`data: ${JSON.stringify({ id: "x", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\n`);
    res.write(`data: ${JSON.stringify({ id: "x", choices: [], usage: { prompt_tokens: 40, completion_tokens: words.length } })}\n\n`);
    res.write("data: [DONE]\n\n");
    res.end();
    return;
  }
  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: { message: `no route ${req.method} ${url.pathname}` } }));
}).listen(port, "127.0.0.1", () =>
  console.log(`mock OpenAI server on http://localhost:${port}/v1 — Chat Completions + Responses (models: ${models.join(", ")}; key "bad" is rejected)`),
);
