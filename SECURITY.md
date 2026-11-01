# Security Policy

OpenGent holds API keys, spawns local processes, and lets AI models call tools on your
machine, so security is a core design goal rather than an afterthought. The codebase is
reviewed periodically for security and performance, and security fixes take priority over
feature work.

## Supported versions

OpenGent is pre-1.0. Only the latest commit on `main` receives security fixes. There are no
release branches or backports.

| Version         | Supported |
|-----------------|-----------|
| `main` (latest) | Yes       |
| Anything older  | No        |

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
| **Network** | Plain HTTP accepted for a non-loopback host, redirects followed on remote MCP connections, or TLS verification bypassed without the user choosing it. |
| **Dependencies** | A known vulnerability in a pinned dependency that is reachable from OpenGent. |

**Out of scope (by design):**

- What a trusted MCP server does once you've connected it. Tool permissions are a consent
  layer, **not** an OS sandbox, so a stdio server runs with your user account's privileges.
- Unencrypted local chat data. It relies on OS account isolation and disk encryption;
  per-chat encryption is a planned feature.
- Attacks that need an already-compromised user account or a malicious local administrator.
