# Feature guide

Everything OpenGent can do today, organized by where you find it in the app. This is the
reference for **what exists and how to use it**. It is not a history: what changed and when is
in the [changelog](../CHANGELOG.md), what's planned is on the [roadmap](../README.md#roadmap),
and the security model is in [SECURITY.md](../SECURITY.md).

Each feature is described the same way:

| Part | Answers |
|---|---|
| **What** | What the feature does, in a sentence or two |
| **How to use** | Where it is in the app and the steps to use it |
| **How it works** | Behavior worth knowing: what is stored, what is sent, what happens in edge cases |
| **Limits** | Caps, unsupported cases and known gaps |

## At a glance

| Area | Feature | In short |
|---|---|---|
| [Chat](#chat) | [Conversations](#conversations) | Streaming replies with Stop, Regenerate, copy and live token usage |
| | [Rich rendering](#rich-rendering) | Sanitized Markdown, code highlighting, KaTeX math, reasoning blocks |
| | [Attachments](#attachments) | Text files, images (PNG, JPEG, GIF, WebP) and PDFs in a prompt — picker or drag-and-drop |
| | [History & search](#history--search) | Chats saved locally, searchable, recovered after a crash |
| [Models & providers](#models--providers) | [Provider types](#provider-types) | OpenAI, Google Gemini, Anthropic, xAI, any OpenAI-compatible server, Simulated |
| | [Provider settings](#provider-settings) | Add, test and manage connections; keys go to the OS keychain |
| | [Model list & hiding](#model-list--hiding) | Discovered models, per-model context windows, hide the ones you don't use |
| | [Reasoning continuity](#reasoning-continuity) | A model's reasoning carries across tool calls |
| | [Environment context](#environment-context) | Models know your OS, time zone and when each message was sent |
| [Tools](#tools-mcp-connections) | [Connections](#connections) | MCP servers over stdio or Streamable HTTP, with OAuth sign-in or client credentials |
| | [Tool permissions](#tool-permissions) | *Allow* / *Ask* / *Disable* per tool, enforced in Rust |
| | [Scopes](#permission-scopes) | Defaults per connection, overrides per chat, own set per agent |
| | [Approvals](#approvals) | Allow once / Always allow / Deny on a card showing the arguments |
| | [Tool loading](#tool-loading) | All, on-demand (`find_tools`), or automatic |
| [Agents](#agents) | [Agents](#agents) | Reusable role with models, instructions and its own tools |
| | [Fallback model](#fallback-model) | A second model takes over when the first can't start |
| | [Schedules](#schedules) | Run an agent once, on an interval, or on weekdays |
| [Command line](#command-line) | [`opengent`](#command-line-opengent) | List models and connections, run prompts and agents, manage schedules from a terminal |
| [Settings](#settings) | [Settings dialog](#settings-dialog) | App-wide preferences and About details in one dialog, ⌘, / Ctrl+, |
| [Privacy & data](#privacy--data) | [Local data](#local-data) | Everything under `~/.opengent`; no account, no telemetry |
| | [Secrets](#secrets) | API keys and tokens only in the OS credential store |
| [Appearance](#appearance) | [Activity indicator & theme](#activity-indicator--theme) | Live run phases, System / Light / Dark theme setting, reduced motion |
| | [Window size](#window-size) | Opens at about 80% of the screen, centred, at any display scaling; replies widen in large windows |

Keyboard shortcuts: **⌘N / Ctrl+N** new chat · **⌘, / Ctrl+,** settings · **Esc**
stop the current reply or close a dialog · **Shift+Enter** new line in the composer.

---

## Chat

### Conversations

**What.** A conversation with the model you pick, streamed token by token. Each reply shows
its real phases — generating, awaiting approval, calling a tool — as they happen.

**How to use.**

- Type in the composer and press Enter. Choose the provider and model in the composer's model
  picker; the choice applies per message, so you can switch models mid-chat.
- **Stop** (or Esc) cancels a reply. **Regenerate** retries the last one.
- Copy a whole reply with the button under it, or a single code block with the button on the
  block.
- Scroll up at any time to read earlier text while a reply streams; **Jump to latest ↓** (or
  scrolling back to the bottom) resumes following.
- Click a chat's title to rename it.

**How it works.** The chat history is held locally and the model receives it on every turn.
When the history no longer fits the model's context window, the oldest turns are left out and
the reply shows an "older messages trimmed" notice. Token usage is shown when the provider
reports it. A streaming caret marks where the next token will appear, including inside lists,
tables, blockquotes and code blocks.

**Limits.** One reply at a time per chat. Trimming is by whole turns; there is no
summarization of older history yet.

### Rich rendering

**What.** Replies render as Markdown: headings, lists, tables, links, syntax-highlighted code
blocks, and math via KaTeX (`$…$`, `$$…$$`, `\(…\)`, `\[…\]` and math fences). Models that
expose reasoning get collapsible reasoning blocks inline with the reply, one for each
reasoning step, in the order they happened (for example: thinking, tool call, thinking, answer).
Each block is labeled **Thinking…** while the model reasons and **Thoughts** once it's done.

**How it works.** Rendering is sanitized: raw HTML in model output is disabled and the result
is filtered again before display. Links open in your default browser — the app window never
navigates. A reasoning block is open while the model reasons and collapses once it moves on to a
tool call or the answer. Long reasoning scrolls inside the block and follows the newest text;
scroll up inside it to read earlier reasoning, and scroll back to its bottom to resume
following. Code in a reasoning block wraps to the block's width; in replies, wide code scrolls
sideways inside its block, and wide tables and display math scroll sideways in both. A formula
whose closing delimiter hasn't arrived yet shows as dimmed source and renders as math once it
does — including when a reply ends mid-formula after being stopped or cut off. A complete
formula that's still invalid shows a parse error. Saved chats reopen with the same layout.

**Limits.** No raw HTML, embedded media or diagrams. What appears in a reasoning block depends
on the provider: some send full reasoning, most hosted models send a summary.

### Attachments

**What.** Attach text files, images and PDFs to a message. Text is included in the prompt
as labeled excerpts; images go to models that can see them; PDFs are sent as pictures of
their pages, so the model sees layout, tables, charts and scans as well as the words.

**How to use.** Click **⌁ Attach** and pick up to 20 files, or drag files from your file
manager and drop them anywhere on the chat — an overlay confirms the drop while you hover.
Dropped files follow the same rules as picked ones. Attached files appear as chips (images
as thumbnails, PDFs with a small picture of page 1 and their page count) in the composer and
in the sent message. While a PDF is being turned into page images the composer says so
(*Rendering report.pdf…*). Files that can't be used are skipped individually, each with a
reason. Drops are ignored while the Agents view or a settings dialog is open.

| Kind | Accepted | Notes |
|---|---|---|
| Text | Any UTF-8 text file | SVG counts as text |
| Images | PNG, JPEG, GIF, WebP | Needs a vision-capable model |
| PDFs | Any PDF that opens without a password | Needs a vision-capable model; the first 20 pages are sent |
| Not yet | HEIC/HEIF, BMP, TIFF, Office and other documents, non-UTF-8 text | Skipped with a reason |

**Which models accept images.** PDFs follow the same rule, because their pages are sent as
images. Decided per provider and model, in this order:

1. **Your setting** — Provider settings → Advanced → *Image input*: *Auto-detect* (default),
   *Supported*, or *Not supported*. Anything other than Auto-detect wins outright.
2. **What the server reports** — detected when you Save & test a provider or refresh its
   models (native providers, OpenRouter, LM Studio and Ollama report it). Capable models are
   marked "· vision" in the picker.
3. **The model's name** — well-known vision families (GPT-4o and later, Claude 3 and later,
   Gemini, LLaVA, Qwen-VL, Pixtral, Gemma 3 and similar) count as capable even when the
   server says nothing.

**How it works.** Files are copied into `~/.opengent/attachments` when attached, so later
edits to the original don't change the chat. Identical files are stored once. Images and
PDFs are identified by their content, not their extension. Images and PDFs picked while a
text-only model is selected are skipped straight away. If you switch to a text-only model
with images or PDFs pending, they are flagged and Send is blocked until you remove them;
reopening an old chat with images or PDFs on a text-only model replaces each one with a
labeled placeholder rather than dropping it silently.

A PDF is turned into page images once, when you attach it, by the PDF renderer built into
the operating system (CoreGraphics on macOS, Windows.Data.Pdf on Windows), running inside
the app itself. Each page is drawn on a white background at up to 1,568 pixels on its longer
side and about 1.15 megapixels in total — roughly what vision models look at anyway — and
stored next to the original, so every later message in the chat reuses the same images.
Each page reaches the model labeled with its document and page number, so pages from
several PDFs in one message stay apart. The chip shows what will be sent: *PDF · 12 pages*,
or *PDF · pages 1–20 of 57* when the document is longer than the page limit; the model is
told about the cut too. Attaching the same PDF again reuses the stored pages, unless the
earlier import was cut short (see below) — then it is rendered again.

One request carries at most 100 images and about 16 MiB of image data, counting attached
images and PDF pages together, so a chat full of PDFs stays within what providers accept.
The newest messages keep theirs; once the allowance is spent, pages from older messages are
left out and the model is told which attachments were omitted. Attach a file again to bring
it back into view.

**Limits.** 20 files per message. Text files and images: 2 MiB each. PDFs: 32 MiB each.
Each image counts as roughly 1,500 tokens of context, and so does each PDF page — a 20-page
PDF is about 30,000 tokens, re-sent with every message while it is in the chat's history,
so long PDFs need a model with a large context window. Only the first 20 pages of a PDF are
sent; split a longer document to cover the rest. Rendering stops early if a PDF takes more
than 30 seconds, keeping the pages finished by then; the time is checked between pages, so
a single very complex page can take longer. Password-protected and damaged PDFs are skipped
with a reason, as are files that do not begin with the PDF marker `%PDF-`. Text in a PDF is
read by the model from the page images, not extracted, so text-only models cannot use PDFs
yet and very small print may be misread. Each PDF keeps its original plus its page images
on disk (typically 2–8 MiB for 20 pages); attachments are not deleted from disk yet, even
when their chat is. A dropped PDF is rendered before the app checks whether the chat's model
can use it. On Linux and other platforms without a built-in renderer, PDFs are skipped.

### History & search

**What.** Every chat is saved locally as it streams and listed in the sidebar. The app opens
on a fresh chat.

**How to use.** Use **Search chats** in the sidebar to search titles, message text and
reasoning. Rename a chat by clicking its title; delete it from the sidebar.

**How it works.** A reply is saved while it streams, so quitting or crashing mid-reply loses
nothing already received: on the next launch the turn is shown as interrupted and can be
regenerated. Search does not match tool arguments or internal ids.

**Limits.** No export/import, folders or pinning yet. Deleting a chat is permanent.

---

## Models & providers

### Provider types

A **provider** is a saved connection to a model service. You can add several of the same type
(for example personal and work keys).

| Type | Connects to | Key | Reasoning shown | Model limits |
|---|---|---|---|---|
| **OpenAI** | OpenAI Platform (Responses API) | API key | Summaries | Built-in table per model family |
| **Google Gemini** | Google AI Studio (Interactions API) | API key | Thought summaries | Reported by Google |
| **Anthropic** | Anthropic API (Messages API) | API key | Thinking summaries | Reported by Anthropic |
| **xAI** | xAI API (Responses API) | API key | Detailed summaries | Reported reasoning, built-in context table |
| **OpenAI-compatible** | Any Chat Completions server: Ollama, LM Studio, vLLM, llama.cpp, OpenRouter, a company gateway | Optional | When the server streams it | Set by you, or reported |
| **Simulated** | Nothing — built in | None | — | — |

All types support streaming, tool calling (including several calls in one step) and image
input where the model does. More native providers are on the
[roadmap](../README.md#native-providers).

#### OpenAI

**What.** GPT and o-series models with reasoning summaries, tools and image input.

**How to use.** Create a key at [platform.openai.com](https://platform.openai.com) → *API
keys*. Choose the **OpenAI** tile, paste the key, **Save & test**, then pick a default model.

**How it works.** OpenGent uses the Responses API because OpenAI's newer models restrict or
drop tool calling on Chat Completions. Requests are stateless (`store: false`): the full local
history is sent each turn and OpenAI keeps no conversation. OpenAI doesn't report model
limits, so context windows and output caps come from a built-in table; your own setting
always wins.

**Limits.** API keys only — no Azure OpenAI, organization/project headers or OAuth. OpenAI's
built-in tools (web search, file search, code interpreter, computer use) and structured
outputs aren't offered. Reasoning effort is the model default. A dropped connection fails the
turn. Models newer than the built-in table use your context-window setting, or 8K.
To use Chat Completions against OpenAI instead, add an *OpenAI-compatible* provider with the
same URL.

#### Google Gemini

**What.** Gemini models, and Gemma models served by the Gemini API, with thought summaries,
tools and image input.

**How to use.** Create a key in [Google AI Studio](https://aistudio.google.com) → *Get API
key*. Choose the **Google Gemini** tile, paste the key, **Save & test**, then pick a default
model.

**How it works.** Requests are stateless (`store: false`): the full local history is sent
each turn and Google keeps no conversation. Thought tokens are counted as output in usage.
If Google's stream closes early on a long reply, OpenGent resumes or retries it once before
reporting an error.

**Limits.** AI Studio API keys only — no Vertex AI, OAuth or application default
credentials. Google's built-in tools (Search, code execution, URL context) and structured
output aren't offered. Thinking level is the server default. A resumed stream may briefly
repeat text already shown.

#### Anthropic

**What.** Claude models with thinking summaries, reasoning between tool calls, tools and
image input.

**How to use.** Create a key in the [Anthropic Console](https://console.anthropic.com) →
*API keys*. Choose the **Anthropic** tile, paste the key, **Save & test**, then pick a default
model.

**How it works.** The Messages API is stateless: the full local history is sent each turn and
Anthropic keeps no conversation. Claude 4.6 and later decide for themselves how much to
think; older models answer directly. A refusal is reported with Anthropic's explanation, which
lets an agent's fallback model take over.

**Limits.** API keys only — no Bedrock, Vertex or OAuth. Anthropic's built-in tools (web
search, code execution), citations, batch and prompt caching aren't offered (caching is
[Phase 6](../README.md#roadmap)). Thinking budgets can't be set. A dropped connection fails
the turn. On the Claude 5.1 generation, trimming old history can discard earlier reasoning —
the turn still succeeds, with less continuity. Zero data retention is arranged with Anthropic
per organization; see [SECURITY.md](../SECURITY.md#data-handling-by-provider).

#### xAI

**What.** Grok models with tools and image input; reasoning models keep their thinking
across tool calls.

**How to use.** Create a key in the [xAI Console](https://console.x.ai) → *API keys*.
Choose the **xAI** tile, paste the key, **Save & test**, then pick a default model.

**How it works.** xAI speaks the Responses protocol, so the adapter is the same one OpenAI
uses with xAI's own endpoints and model rules. Requests are stateless (`store: false`):
the full local history is sent each turn and no conversation is stored for later retrieval.
xAI still keeps API requests for 30 days for abuse review unless your team has Zero Data
Retention turned on; see [SECURITY.md](../SECURITY.md#data-handling-by-provider). Reasoning
models return their thinking as encrypted reasoning items, which are kept locally and
replayed so Grok stays aware of its earlier reasoning across turns. Models are listed with
the vision and reasoning flags xAI reports; context windows come from a built-in table per
model family and your own setting always wins. A single request may run up to 15 minutes,
as xAI recommends for high-effort reasoning.

**Limits.** API keys only — no management keys, OAuth or US regional endpoint preset
(you can enter `https://us.api.x.ai/v1` as the base URL yourself). xAI's built-in tools
(web and X search, code execution), file search, batches, prompt caching (planned for
[Phase 6](../README.md#roadmap)) and `previous_response_id` chaining aren't offered.
Reasoning effort is the model default. A dropped connection fails the turn. Grok's thinking
content stays encrypted — only its summary (when returned) is readable. Images must be JPEG
or PNG; GIF and WebP attachments are replaced by a note telling the model they were
omitted. Models newer than the built-in table use your context-window setting, or 8K.
**Cost:** xAI bills a request whose prompt reaches 200K tokens at about twice the normal
rate for all of its tokens. OpenGent fills most of Grok's 500K–2M windows before trimming
history, so a long chat crosses that threshold without warning — set a lower context window on the provider or
model to stay under it.

#### OpenAI-compatible

**What.** Any server that speaks the OpenAI Chat Completions protocol — the way to run fully
local models or to go through a gateway your organization approves.

**How to use.** Choose the **OpenAI-compatible** tile and either pick a *Quick setup* preset
or enter a base URL ending in `/v1`.

| Preset | Base URL |
|---|---|
| Ollama | `http://localhost:11434/v1` |
| LM Studio | `http://localhost:1234/v1` |
| vLLM / llama.cpp | `http://localhost:8000/v1` |
| OpenRouter | `https://openrouter.ai/api/v1` |

An API key is only needed for hosted gateways or servers started with one.

**How it works.** Models are discovered from the server's `/models` list. Reasoning is shown
when the server streams it (`reasoning_content` or `reasoning`).

**Limits.** Capabilities vary by server: tool calling and image input depend on the model and
on what the server implements. Servers that report no context window use your setting, or 8K.

#### Simulated

**What.** A built-in provider that streams canned replies, so you can try the interface with
no account, key or network. It is always available in the model picker.

### Provider settings

**What.** One place to add, test, edit and remove provider connections.

**How to use.** Click **Providers** in the sidebar, or open **Settings** (⌘, / Ctrl+,) and
choose **Providers**. The dialog opens on a picker: one tile per
provider type next to **Your providers**, the list of saved connections.

1. Click a tile. The form asks only for the essentials: name, API key (with *Show* and *Get
   an API key ↗*), default model — plus base URL and presets for OpenAI-compatible servers.
2. **Save & test** saves, connects and lists the provider's models, then stays on the form
   so the discovered list is right there for choosing a default model. **Save** stores the
   connection without testing and returns to the picker.
3. **← All providers** returns to the picker, asking first if you have unsaved edits.

**Advanced settings** holds what most setups never touch:

| Setting | Purpose |
|---|---|
| Base URL (built-in types) | Point a native provider at a proxy or gateway |
| TLS verification | *System* (default), a *pinned CA* certificate, or *skip verification* |
| Context window | A provider-wide cap on how much history is sent |
| Image input | *Auto-detect*, *Supported* or *Not supported* |
| Token usage | Ask the server to report usage (OpenAI-compatible) |
| Models | The model list, per-model context windows and the *shown* toggles |

The section opens by itself, marked *customized*, when a saved provider changes any of these.

**How it works.** API keys go straight to the OS keychain and are never shown again; when
editing a provider, leave the key field blank to keep the stored key. Removing a provider
deletes its key from the keychain. HTTPS is required unless the server is on this machine. Redirects are
never followed, so a key can't be forwarded to another host — if a base URL redirects, the
error tells you to use the final address. A failed save keeps everything you typed.

**Limits.** The picker has no search or categories yet.

### Model list & hiding

**What.** Each provider keeps the list of models it discovered, with their limits. Hide the
ones you don't use to keep the model picker short.

**How to use.** Provider settings → **Advanced settings** → *Models*. Untick *shown* to hide a
model from the chat picker and the agent model dropdowns; set a per-model context window when
the default is wrong.

**How it works.** The context window for a model is the first of: your per-model value → your
provider-wide setting → the value the provider reports → 8K. Hiding is a listing preference,
not a block: the model you currently have selected, and agents or schedules already using a
hidden model, keep working. Hidden models survive a model refresh.

**Limits.** The provider's default model can't be hidden. Changing a provider's type clears
its hidden list.

### Reasoning continuity

**What.** OpenAI, Gemini, Anthropic and xAI return protected reasoning data that must be sent back
on the next request for the model to continue its line of thought across tool calls. OpenGent
stores and replays it for you.

**How it works.** The data is saved locally with the message it belongs to and replayed only
to the same provider family. It is never displayed, never searched, and never sent to a
different vendor. Switching a chat to another provider simply leaves it out — the readable
reasoning text stays in the chat either way.

**Limits.** Continuity applies only while you stay with the same provider family. Unusually
large reasoning blocks (over 64 KiB) aren't stored; the model then reasons afresh on the next
turn.

### Environment context

**What.** Models are told your operating system, your time zone and when each of your messages
was sent, so they can answer questions like "what's due tomorrow?" and give advice that fits
your system. It works in every chat and agent, in scheduled runs, and with or without tools.

**How to use.** On by default. Turn it off with **Share environment with models** in
**Settings → General**. A change applies to replies started after it.

**How it works.** The system prompt gets one short line, for example
`macOS 12.6, America/Toronto. User messages start with their local send time.` On Windows the
version reads like `Windows 11 (build 22631)`. Each of your messages is sent with its local send
time in front, for example `[Sun 2026-11-22 14:35]`, taken from when the message was stored and
shown in the time zone rules in effect at that moment, so a clock change doesn't alter earlier
messages. A scheduled run's prompt carries the time the schedule fired. The stamps are added only
to what the model receives: they aren't saved in the chat or shown in it. Because the system
prompt has no clock and earlier messages never change, requests stay cache-friendly from turn to
turn. With the setting off nothing is collected and requests are exactly as they would be
without this feature.

**Limits.** No hostname, username, home folder, locale, screen or hardware details are sent. If
the OS version can't be read it is left out; if the time zone name can't be read the current UTC
offset (e.g. `UTC-05:00`) is used instead. Turning the setting on in an existing chat also sends
the send times of its earlier messages. A configured fallback model receives the same
information. It costs roughly 20 tokens per request plus about 10 per user message.

---

## Tools (MCP connections)

Tools come from [MCP](https://modelcontextprotocol.io) servers you connect. The model
proposes a call; your permissions decide whether it runs.

### Connections

**What.** A connection is one MCP server: a **local command** (stdio) or a **remote server**
(Streamable HTTP).

**How to use.** Open **Connections** (sidebar → ⚒) and add a server:

| Kind | You provide | Example |
|---|---|---|
| Local command | Command, optional working directory and environment variables | `npx -y @modelcontextprotocol/server-everything` |
| Remote server | URL, optional authentication and headers | `https://example.com/mcp` |

- Paste a standard `{"mcpServers": {…}}` JSON config to prefill the form.
- Mark an environment variable or header as *secret* to keep its value in the OS keychain.
- **Save & test** saves, connects and reports "connected · N tools discovered" or the error,
  keeping the connection open so you can set its tool permissions. **Save** alone saves and
  returns to the Connections start screen; it reconnects only if the command, directory,
  environment, URL, authentication or headers changed.
- A server that provides an icon shows it next to the connection's name in Connections and
  in the ⚒ Tools menu, in its light or dark variant to match the app's theme; otherwise the
  name's first letter is shown. The name you gave the connection is always the label.
- **Server instructions** — some servers send tips on using their tools (for example
  GitHub's on pagination and pull request reviews). A connected server's instructions are
  shown in its connection with an **Add to the system prompt** switch.
- **Authentication** (remote servers) — pick one:
  - *None*: no sign-in, or credentials you add as headers (for example an
    `Authorization: Bearer …` personal access token).
  - *OAuth — sign in with browser*: press **Sign in**; the server's sign-in page opens in
    your browser and the connection comes up when you approve. Leave **Client ID** blank to
    let the app register itself with the server, or enter the ID (and secret, if the
    server issued one) of an app you registered. **Scopes** narrows or widens what you
    grant. **Callback port** fixes the redirect URI `http://127.0.0.1:<port>/callback` for
    servers that need it registered exactly. **Sign out** forgets the sign-in on this
    device; it doesn't revoke the tokens at the server.
  - *OAuth — client credentials*: a client ID and secret are exchanged for a token without a
    browser, for service accounts.
  Custom headers stay available in every mode, except `Authorization` with OAuth.
- The **Enabled** switch turns a connection off everywhere. To turn it off for a single chat
  or agent, use the ⚒ Tools menu instead (see [Scopes](#permission-scopes)).

**How it works.** Servers connect in the background and reconnect when they drop; a status
dot and the server's recent error output show failures. Adding a local command, or changing
its command, working directory or environment, must be confirmed in a native OS dialog, so a
server can never be installed silently. Local servers are stopped, along with any processes
they started, when you disconnect or quit.

OAuth follows the MCP authorization spec: the app discovers the server's authorization
server, uses PKCE, and receives the redirect on a temporary listener on `127.0.0.1` that
closes after the sign-in (or after 5 minutes). Tokens and client secrets are kept in the OS
credential store; signed-in connections reconnect at launch without a browser and refresh
expired tokens on their own. A browser only ever opens when you press **Sign in** —
startup, reconnects and scheduled runs never start one; a connection whose sign-in expired
shows "sign-in required". Client-credentials tokens are renewed shortly before they expire
(retrying briefly if the authorization server can't be reached), and again after a new
secret or scopes are saved.
Errors are reported in one line, such as "This server requires authentication" with what to
do next.

Server icons are used only when the server sends them inline as PNG, JPEG or WebP images;
the app never downloads an icon from a URL. Server instructions are written by the server,
so they are treated as untrusted: invisible and control characters are removed, and they are
sent to the model in a clearly marked block that tells it they are hints about that server's
own tools and can't override its rules, your permissions or your requests. They're included
only in chats and agents that can call at least one of the server's tools. What a server
sends on its first connect is accepted automatically; if it later changes its instructions,
or starts sending them after first connecting without any, they are left out and marked
"changed" until you review them and turn the switch on. Pointing a connection at a different
command, directory or URL counts as a new server, so its first instructions are accepted
again (unless you turned them off).

**Limits.** OAuth endpoints that redirect aren't followed, and an authorization server on a
private or loopback address is only accepted for a server on this machine. Client
credentials support a client secret only (no signed-JWT client authentication) and need
tokens that live longer than about a minute. When a server asks for more scopes, the message
names them; add them and sign in again. Icons over 16 KiB or 512×512 pixels, SVG icons and
icons at a URL aren't shown. Server instructions are kept up to 8,000 characters; the system prompt carries
up to 2,000 per server and 6,000 in total, cut at a line break and marked as truncated. Filtering
can't make instructions safe on its own — tool permissions remain the safeguard, so turn off
instructions from servers you don't fully trust. A local server runs with your user account's privileges —
permissions are a consent layer, not a sandbox, so only add servers you trust.

### Tool permissions

**What.** Every tool has one of three permissions:

| Permission | Effect |
|---|---|
| **Allow** (*Always allow*) | Runs without asking |
| **Ask** | Pauses the reply on an approval card |
| **Disable** (*Block*) | Never offered to the model |

**How to use.** In a connection, **Tool permissions** lists every tool the server reported
with a permission choice, *Default for new tools*, and *Reset all to default*. Changes save
immediately. Each tool carries a badge taken from the server's own description:

| Badge | Meaning |
|---|---|
| Read-only | The server says the tool only reads |
| Write | The server says it changes things, non-destructively |
| Destructive | The server says it can delete or overwrite |
| Unannotated | The server says nothing — treat as potentially destructive; *Ask* is the safe choice |

**How it works.** Permissions are enforced in Rust when a call is dispatched, not in the
interface. Badges are information only and never grant anything. An *Allow* is tied to the
tool's exact definition: if the server later changes the tool's description or inputs, it
drops back to *Ask* and shows a "changed" badge. The tool list is remembered, so permissions
stay editable while a server is disabled or offline. A model that calls a disabled tool is
told it is disabled.

**Limits.** Per server: up to 500 tools; tools with oversized descriptions or input schemas
are skipped.

### Permission scopes

**What.** Connection permissions are the **defaults**. Chats and agents can adjust them
without affecting anything else.

| Scope | Where to edit | Behavior |
|---|---|---|
| **Connection** (defaults) | Connections → Tool permissions | Applies everywhere unless overridden. *Disable* here is a hard ceiling nothing can lift |
| **Chat** | Composer → ⚒ Tools ("Tools · this chat") | Turn a server off for this chat, or set a tool to Allow / Ask / Disable. New chats start with no overrides |
| **Agent** | Agent editor, or ⚒ Tools in an agent chat | The agent has its own tools and permissions (see [Agents](#agents)) |

**How to use.** In the ⚒ Tools menu, overridden tools show a dot; click it, or *Reset to
defaults*, to follow Connections again. Tools disabled in Connections show "off in
Connections".

**How it works.** Every call is re-checked against its scope, so turning a server or tool off
mid-reply blocks its next call. If a chat or agent is deleted while it is running, its
remaining tool calls all require approval.

### Approvals

**What.** When a tool set to *Ask* is called, the reply pauses on an approval card showing
the tool, its arguments and its badge.

**How to use.** Choose **Allow once**, **Always allow**, or **Deny**. *Always allow* is saved
for the current chat or agent only — never to the global defaults.

**How it works.** A denial is reported back to the model so it can continue without the
tool. Calls with malformed arguments are rejected before you are asked.

**Limits.** An unanswered approval is denied automatically after 15 minutes.

### Tool loading

**What.** Controls how many tool definitions are sent with each request — relevant once you
connect many servers.

**How to use.** ⚒ Tools → *Tool loading*:

| Mode | Behavior |
|---|---|
| **Auto** (default) | Sends everything while it's small; switches to on-demand past about 6k tokens of definitions or a quarter of the context window |
| **Always load all** | Sends every enabled tool's definition with each request |
| **On-demand** | Sends only a `find_tools` search tool; the model looks tools up by keyword |

**How it works.** Tools found through `find_tools` stay loaded for the rest of the
conversation. If the model calls a real tool that isn't loaded yet, it is loaded on the spot
and the model retries.

### Tool calls in a reply

**What.** Tool calls appear as cards inside the reply, with status, arguments and result.

**How it works.** The model can call tools, read the results and continue, repeatedly, within
one reply. Results are shortened before they are stored and sent back to the model; older
results are shortened further on later turns. A cancelled or timed-out call is cancelled on
the server too.

**Limits.**

| Limit | Value |
|---|---|
| Tool steps per reply | 8 |
| Calls per step | 16, run one after another |
| Tool result kept | Up to 40k characters or a quarter of the context window; 2k on later turns |
| Images and other binary results | Passed to the model as a text placeholder |

---

## Agents

**What.** An agent is a reusable assistant: a name and description, a default model with an
optional fallback, your instructions, and its own selection of tools and permissions. Agent
chats look like regular chats, labeled with the agent's name.

**How to use.** Click **Agents** in the sidebar, then *Create your first agent* (or open an
existing card with **Configure**). The editor has four sections:

| Section | Contents |
|---|---|
| Identity | Name and description |
| Models | Default model, optional fallback |
| Instructions | Your system prompt for the agent |
| Tools | Tick a connection to add it; **Edit tools & permissions…** turns each tool on or off and sets *Allow* or *Ask*; *New tools* decides what happens to tools the server adds later (*Ask* or *Off*) |

**Save & start chat** opens a new chat with the agent; **Chat** on a card does the same. The
same tool controls are available in an agent chat's ⚒ Tools menu and save straight to the
agent.

**How it works.** Adding a connection copies its current defaults: read-only tools keep
*Allow*, everything else starts at *Ask*, because agents can run unattended. The card shows
how many tools run without asking and highlights when some of them can make changes. *Reset
to connection defaults* copies them again. Your instructions are added to a built-in agent
prompt that covers operating rules and the handling of untrusted tool output. An agent only
sees the connections you gave it, and tools disabled in Connections stay disabled.

**Limits.** An agent can't auto-allow tools a server adds later; each *Allow* is an explicit
choice. Turning a connection off and on again in an agent re-copies the defaults. Image
attachments follow the default model's capabilities. Deleting an agent deletes its schedules
and stops its running chats; the chats themselves remain as plain chats.

### Fallback model

**What.** If the default model can't start a reply — connection, authentication or model
error, or a refusal — the agent's fallback model answers instead.

**How it works.** The switch happens at most once per reply, and the reply is labeled with
the model that produced it. History is sized for the smaller of the two models' context
windows.

**Limits.** No switch after the first model has produced output, since that would duplicate
text.

### Schedules

**What.** Run an agent with a preset prompt automatically. Each run opens a new chat, marked
⏱ in the sidebar.

**How to use.** In a saved agent's editor, **Add schedule** and set the prompt, the repeat
rule and the missed-run policy. Each schedule shows its next run and last outcome (with a link
to the chat), and has a pause toggle, **Run now**, Edit and Delete.

| Repeat | Fires |
|---|---|
| Once | At a date and time |
| Interval | Every N minutes, hours or days, counted from when it was saved or last ran |
| Weekdays | On chosen days at a local time (daylight-saving aware) |

| Missed-run policy | When the app was closed or the computer asleep |
|---|---|
| Run it once when the app is back | Any number of missed runs become a single catch-up run |
| Skip it | Waits for the next scheduled time |

**How it works.** A run counts as missed when it is noticed more than 2 minutes late. At
launch, the scheduler waits briefly for connections so catch-up runs have their tools. The
agent is told the run is unattended and not to wait for answers. If you have the chat open
you see the run live and can answer its approval prompts.

**Limits.** Schedules fire only while the app is open. A tool that needs approval is denied
after 15 minutes if nobody answers. No OS notifications yet.

---

## Command line

### Command line (`opengent`)

**What.** A standalone `opengent` binary for headless use: list your models and MCP
connections, run a prompt (optionally with an agent) and print the reply, and manage agent
schedules. It uses the same data folder, providers, connections, permissions and keychain
entries as the app, and works with the app open or closed.

**How to use.** Installing it, every option, scripting with `--json` and troubleshooting are
in the [command-line guide](cli.md); in short:

| Command | Does |
|---|---|
| `opengent models [--json]` | Lists providers and their models; *active* marks the model last picked in the app |
| `opengent mcp list [--tools] [--json]` | Lists connections (name, id, transport, on/off, default permission); `--tools` adds each server's last known tools and their connection-level permission |
| `opengent run [options] <prompt>` | Runs one prompt. The reply goes to stdout; notices and tool activity go to stderr. Without a prompt argument, piped stdin is read |
| `opengent schedule add --agent <name> (--at … \| --every …) [--skip-missed] <prompt>` | Adds an agent schedule. `--at "2026-12-01 09:00"` or `--at 09:00` (local time); `--every 30m`, `2h`, `1d`, `weekdays@09:00`, `mon,thu@18:30`, `daily@07:15` |
| `opengent schedule list [--agent <name>] [--json]` / `schedule remove <id>` | Lists or deletes schedules |
| `opengent help` / `version` | Usage (also `--help` or `-h` after any command) and the version |

`run` options:

- `--model <provider>/<model>` — provider name (case-insensitive) or id, then the model;
  `--model <provider>` uses its default model. Without it, a continued chat keeps its model,
  and a new one uses the model last picked in the app.
- `--agent <name>` — runs with an agent: its models, servers, tools and permissions.
- `--chat <id>` — continues a chat. A new chat's id is printed on stderr. Chats made from the
  command line are ordinary chats in the app's sidebar.
- `--mcp <server>` (repeatable) — only these connections are on for the chat.
  `--tool <server>/<tool>` (repeatable) — only these tools of that server; its other tools are
  disabled for the chat. `--no-mcp` — no connections. These work like the chat's ⚒ Tools
  menu and are saved on the chat, so the app shows what the run used and a later `--chat` run
  keeps it. They only select: they never set Allow or Ask, and can't turn on a server or tool
  that is off in Connections. Agent runs can't use them.
- `--json` — prints every run event as one JSON line (the same events the app receives).

Exit codes: `0` the reply was stored complete, `1` the run or command failed, `2` usage
error, `3` cancelled (Ctrl-C, a termination signal, or the output was closed).

`OPENGENT_DATA_DIR` points it at another data folder, like the app. `OPENGENT_LOG` (for
example `debug`) adds diagnostics on stderr; without it only warnings are shown.

**How it works.**

- **Permissions** are the ones stored in the app. A tool set to *Allow* (in Connections, a
  chat's Tools menu or an agent) runs without a prompt, also without a terminal — as long as
  its definition matches the one approved in the app. A tool whose definition changed, or that
  the app has never listed, asks instead and a note says to review it in Connections.
  *Disable* is never offered. *Ask* prompts on the terminal for **allow once** or **deny**
  when stdin and stderr are both terminals and `--json` isn't used; otherwise the call is
  denied immediately and named on stderr. There is no *Always allow* on the command line.
  A prompt whose arguments are too long to show in full says how much is hidden.
- **Configuration is read-only.** `providers.json`, `mcp.json` and `settings.json` are never
  written, so the command line can't undo a change made in the app meanwhile; servers,
  permissions and settings are edited in the app. App settings such as *Share environment
  with models* apply to command-line runs.
- **MCP**: only the servers the run can use are connected, for at most 30 seconds; a server
  that isn't ready by then is left out with a warning. Nothing is connected when the model
  can't call tools. Server instructions the app hasn't accepted are left out.
- **Sharing the data folder.** One reply is generated per chat at a time, across the app and
  every command-line process: a second run in a busy chat fails with a clear message. Opening
  the app during a command-line run (or the reverse) doesn't interrupt the other's reply.
- **Agents** run unattended when nobody can answer prompts: they are told not to wait for the
  user.
- **Output.** On a terminal, control characters and escape sequences in model output, tool
  names and arguments are removed — and invisible characters in names — so a tool can't
  disguise what an approval prompt asks or rewrite earlier lines. Piped stdout and `--json`
  are passed through unchanged. A reader that goes away (`| head`) ends the command quietly.
- **Ctrl-C** cancels the run and waits for the cancellation to be saved; a second Ctrl-C quits
  at once. SIGTERM and SIGHUP (Ctrl-Break on Windows) do the same, so a run stopped by
  `timeout` or a service manager is stored as cancelled rather than left unfinished. While
  servers are still connecting, any of them stops the command before a chat is created.
- `models` and `mcp list` read only the config files: no database, no connection, no keychain
  access, so they work while the database is busy or before it exists.

**Limits.**

- Not included in the release downloads yet; build it from source
  (`cargo install --path crates/cli --locked`, see the [guide](cli.md#install)).
- On macOS a new or rebuilt (unsigned) binary triggers a keychain prompt the first time it
  reads a stored key.
- Connections can't be added or changed, and nothing can be attached to a prompt.
- Schedules are stored for the app, which fires them while it is open; there is no headless
  scheduler.
- If a process is killed mid-reply while another one keeps the data folder open, that chat
  reports busy until a process next starts alone.
- OAuth token refreshes aren't coordinated between processes: if the app and a command-line
  run refresh the same connection at the same moment against a server that rotates refresh
  tokens, one of them can be signed out (sign in again in the app).

---

## Settings

### Settings dialog

**What.** One dialog for app-wide preferences, with a list of sections on the left and the
selected section on the right.

**How to use.** Click **Settings** at the bottom of the sidebar, or press ⌘, (macOS) /
Ctrl+, (Windows); the shortcut closes it again, as do Esc and a click outside. Sections:

| Section | What's there |
|---|---|
| **General** | *Models*: **Share environment with models** ([environment context](#environment-context)). *About*: the app version, the data folder (with *Copy*) and where secrets are stored (macOS Keychain or Windows Credential Manager) |
| **Appearance** | *Theme*: System, Light or Dark (see [Activity indicator & theme](#activity-indicator--theme)) |
| **Providers** | Opens the [provider settings](#provider-settings) dialog |
| **Connections** | Opens the [Connections](#connections) dialog |

**How it works.** Changes apply as soon as you make them; there is no Save button. If a
change can't be saved, the control returns to its previous value and the reason is shown
next to it. Preferences are kept in `settings.json` in the data folder and contain no
secrets. If the file is missing the defaults are used;
if it is damaged it is kept next to the original as `settings.json.corrupt-<time>` and the
app starts with defaults.

**Limits.** Later features add their own sections. Providers and Connections still open as separate dialogs. There is no search, import or
export, and settings are app-wide (not per chat or agent).

---

## Privacy & data

### Local data

**What.** OpenGent has no account, no cloud service of its own and no telemetry. The only
network traffic is to the providers and MCP servers you configure.

| Data | Location |
|---|---|
| Chats, messages, agents, schedules | SQLite database in `~/.opengent` |
| Attachments | `~/.opengent/attachments` |
| Provider and connection settings | JSON files in `~/.opengent` (no secrets) |
| App-wide preferences | `~/.opengent/settings.json` (no secrets) |
| API keys, secret headers and variables, OAuth tokens and client secrets | OS credential store |

**How it works.** With a local model (Ollama, LM Studio, vLLM, llama.cpp) nothing leaves
your machine. Hosted providers receive the conversation for each request and are used in
their stateless mode; what each vendor may retain is covered in
[SECURITY.md](../SECURITY.md#data-handling-by-provider). Unless you turn off
[environment context](#environment-context), requests also include your OS version, time zone
and the send time of each of your messages.

**Limits.** Local chat data isn't encrypted by the app; it relies on your OS account and disk
encryption. Per-chat encryption is on the [roadmap](../README.md#proposed-features).

### Secrets

**What.** API keys and secret connection values live only in the macOS Keychain or Windows
Credential Manager.

**How it works.** Secrets never reach the interface, the database, settings files or logs.
Settings record only *that* a key exists. To replace a key, type a new one over it; removing
a provider deletes its key.

---

## Appearance

### Activity indicator & theme

**What.** A radar symbol next to each reply spins while the model is working and stops when
the reply ends; the label beside it names the current phase. The app follows your system's
light or dark theme unless you choose one for the app.

**How to use.** Open Settings → Appearance and pick a *Color mode*: **System** (the default)
follows your OS, **Light** and **Dark** keep the app in that mode whatever the OS uses. The
change applies at once, with no restart, and is remembered across restarts.

**How it works.** The choice is applied to the app window itself, so everything that
follows light or dark follows it: colors, code highlighting, the light and dark variants of
MCP server icons, and on Windows the title bar. It is saved in `settings.json` in the data
folder. Turning on your OS's reduced-motion setting stops the rotation.
Scrollbars are thin overlays drawn over the content and shown while you scroll, on both
macOS and Windows.

**Limits.** The indicator shows activity, not progress. The theme is app-wide: there are no
per-chat themes or custom accent colors. Overlay scrollbars on Windows need WebView2 Runtime 125 or newer; older runtimes keep the
classic scrollbar.

### Window size

**What.** The app window opens at a size that fits the screen it appears on, centred in the
area not taken by the taskbar or Dock.

**How it works.** The window takes about 80% of the screen's usable width and height, measured
after display scaling, so a 4K screen at 300% gets the same proportion as a 1080p screen at
100%. It is never smaller than 900 × 600 unless the screen itself is smaller, in which case it
fills the usable area. Resize it freely once it is open.

The conversation keeps a comfortable reading width at the default size. In a larger or
maximized window it widens gradually, up to about 160 characters per line, so code, tables
and lists get more room. The message box keeps the same width at every size. Like the window,
this is measured after display scaling: a 4K screen at 300% looks the same as a 1080p screen,
and at 100% it gets the widest conversation.

**Limits.** The last size and position are not remembered; every launch starts at the default.
