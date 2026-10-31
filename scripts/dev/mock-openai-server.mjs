// Minimal OpenAI-compatible server for manual testing without a real model.
// Usage: node scripts/dev/mock-openai-server.mjs [port]
// Then add a provider with base URL http://localhost:<port>/v1 (default 8787).
import { createServer } from "node:http";

const port = Number(process.argv[2] ?? 8787);
const models = ["mock-fast", "mock-slow", "mock-vision"];

createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://localhost:${port}`);
  if (req.method === "GET" && url.pathname === "/v1/models") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ object: "list", data: models.map((id) => ({ id, object: "model" })) }));
    return;
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
}).listen(port, "127.0.0.1", () => console.log(`mock OpenAI server on http://localhost:${port}/v1 (models: ${models.join(", ")})`));
