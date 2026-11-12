# Security Policy

OpenGent holds API keys, spawns local processes, and lets AI models call tools on your
machine, so security is a core design goal rather than an afterthought. The codebase is
reviewed periodically for security and performance, and security fixes take priority over
feature work.

## Supported versions

OpenGent is pre-1.0. Security fixes land on `main` and ship in the next release; there are no
release branches or backports, so update to the latest release to get them.

| Version                     | Supported |
|-----------------------------|-----------|
| Latest release and `main`   | Yes       |
| Older releases              | No        |

## Reporting a vulnerability

**Please do not open a public issue, discussion, or pull request for security reports.**

Report privately through GitHub: go to the repository's **Security** tab and choose
**Report a vulnerability**
([GitHub docs](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)).

Please include:

- the affected commit or version, and your OS
- steps to reproduce, or a proof of concept
- the impact you observed or expect

OpenGent has a single maintainer, so responses are best-effort. Expect an acknowledgement
within a week. Once a fix lands on `main`, the advisory is published with credit to you
(unless you'd rather stay anonymous).

## Scope

Read the [security model](README.md#security) first. These are the trust boundaries that
matter:

| Boundary | In scope |
|---|---|
| **Secrets** | Any path that leaks an API key, token, or secret env/header value to the webview, logs, SQLite, `mcp.json`, or an unintended endpoint. Secrets should only ever exist in the OS keychain and in Rust memory. |
| **Tool permissions** | Bypassing *Always allow / Ask / Block* enforcement: a tool running without the approval it requires, a blocked tool reaching the model, or an *Always allow* grant surviving a change to the tool's definition. |
| **Process spawning** | Starting or changing a stdio MCP server without the native confirmation dialog, environment leakage beyond the allow-list, or child processes outliving their connection. |
| **Webview** | Escaping the CSP or capability set to get shell, filesystem, or network access from the UI. |
| **Network** | Plain HTTP accepted for a non-loopback host, redirects followed on provider or remote MCP connections, or TLS verification bypassed without the user choosing it. |
| **Dependencies** | A known vulnerability in a pinned dependency that is reachable from OpenGent. |

**Out of scope (by design):**

- What a trusted MCP server does once you've connected it. Tool permissions are a consent
  layer, **not** an OS sandbox, so a stdio server runs with your user account's privileges.
- Unencrypted local chat data. It relies on OS account isolation and disk encryption;
  per-chat encryption is a planned feature.
- Attacks that need an already-compromised user account or a malicious local administrator.

## Data handling by provider

OpenGent is the system of record for conversations: chats live only in the local SQLite
database, and every provider is driven **statelessly**. The full, locally held history is
sent with each request, and no server-side conversation features are used — no stored
responses or interactions, no `previous_response_id`-style chaining, no background runs,
batches or file uploads.

What still differs between providers is **who processes a request and what they may keep
afterwards**. OpenGent groups that into four levels. They are this project's shorthand for
data exposure, not certifications — a formal compliance requirement (SOC 2, HIPAA, GDPR, a
ZDR contract) is settled between you and the vendor.

| Level | Name | What it means |
|---|---|---|
| **1** (highest) | Private | The model runs on your machine or on infrastructure you control. No third party receives, processes or stores a chat. |
| **2** | Hosted, zero retention | A vendor processes each request, under a zero-data-retention (ZDR) arrangement between your organization and that vendor: nothing is kept once the response is returned. |
| **3** | Hosted, stateless | A vendor processes each request and stores no conversation through the API, but its standard terms still apply — typically time-limited logs for abuse monitoring. |
| **4** | Hosted, unverified | A third-party gateway or server whose retention, and that of whatever it forwards to, OpenGent can't know. |

| Provider | Processed by | How OpenGent calls it | Default level | Best level available |
|---|---|---|---|---|
| **Local model** — Ollama, LM Studio, vLLM, llama.cpp via *OpenAI-compatible* on loopback or your own network | You | Chat Completions, stateless | **1** | **1** |
| **Simulated** | Nobody (no network) | — | **1** | **1** |
| **OpenAI** | OpenAI | Responses API with `store: false` | 3 | **2**, once OpenAI has approved ZDR for your organization |
| **Anthropic** | Anthropic | Messages API (stateless by design) | 3 | **2**, with a ZDR arrangement for your organization — see the model exceptions below |
| **Google Gemini** | Google | Interactions API with `store: false` | 3 | 3 with an AI Studio key, the only credential supported today; check Google's terms for your account tier |
| **Hosted gateway** — OpenRouter or another *OpenAI-compatible* service | The gateway and the vendors behind it | Chat Completions, stateless | 4 | Whatever the gateway's terms and routing guarantee |
| **Company gateway** — an *OpenAI-compatible* endpoint your organization runs | Your organization, plus anything it forwards to | Chat Completions, stateless | Set by your organization | **1** when it serves internal models only |

Per provider:

- **OpenAI.** Every request sends `store: false`, so no response or conversation item is
  stored for later retrieval. Encrypted reasoning items are returned to the app and replayed
  from local storage instead of being referenced by id, which is the pattern a ZDR
  organization needs.
- **Anthropic.** The Messages API keeps no conversation state, so there is no `store` flag to
  set; the adapter also uses none of the features that hold data server-side (batches, file
  uploads, Anthropic-hosted tools, prompt caching). ZDR is an organization-level arrangement
  with Anthropic, and everything OpenGent uses is eligible for it, with two model-specific
  caveats: Claude Fable and Mythos models require 30-day retention and reject requests from
  ZDR organizations, and thinking blocks from Claude Sonnet 5.5 and Haiku 5.5 only work for
  the account that produced them.
- **Google Gemini.** Every request sends `store: false`. Recovering a dropped stream may ask
  Google for the remainder of the same in-flight response; nothing is read back from earlier
  turns. Only AI Studio API keys are supported (no Vertex AI), and Google's terms treat free
  and paid usage differently — verify them before sending sensitive data.
- **OpenAI-compatible.** Stateless by protocol: the app sends only the request body. What the
  other side keeps is that server's policy. For the strongest guarantee, use a local model.

In every case:

- "Stateless" and `store: false` mean **no API-level storage**. A vendor's own logging,
  abuse monitoring or legally mandated retention still applies under its data-usage terms and
  your account settings. Level 2 needs an agreement with the vendor; the app can't grant it.
- Provider continuation state that must be replayed on the next turn (OpenAI encrypted
  reasoning items, Gemini thought signatures, Anthropic thinking signatures) is stored locally
  with the message it came with, never crosses IPC or appears in search, and is only sent
  back to the provider family that issued it.
- Provider traffic originates in Rust, never the webview. API keys live in the OS keychain.
  HTTPS is required for non-loopback endpoints unless the user explicitly opts out, and
  redirects are never followed.
- MCP servers are separate from model providers: a tool call sends its arguments to that
  server, and tool results are sent to the model like any other message. A remote MCP server
  is a third party in its own right.
