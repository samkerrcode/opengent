# Scoped tool permissions (design)

Status: **proposed** — the second of two PRs. PR A (Connections enable/test flow, the
per-tool permission table, access badges, the cached tool catalog, and pin-at-grant) has
shipped; see [features.md](../features.md#connections-mcp-tools). This document is the
implementation plan for PR B. When PR B ships, move the user-facing parts into
`features.md` and remove the README "Proposed features" row.

## Problem

Tool permissions and a connection's enabled state are global:

- Turning a server off in a chat's ⚒ Tools menu flips the global `enabled` flag, which
  disconnects it from every chat and agent.
- Changing a tool's policy in a chat changes it everywhere.
- Agents filter which tools they can see (`AgentServerAccess.tools`) but inherit global
  policies. An agent's permissions end up depending on whatever the last chat set.

## Goals

1. Connection permissions (Connections dialog) are the **defaults**.
2. The ⚒ Tools menu in a plain chat sets **per-chat overrides**. It never disconnects a
   server; only Connections enables or disables one.
3. Agents **own their permissions**: which servers and tools they use, and Allow/Ask per
   tool. Permissions are copied from the defaults when a server is added. Tools that appear
   later fall back to the agent's "new tools" default.
4. In an agent chat, the ⚒ Tools menu and the approval card's *Always allow* edit the
   **agent**, not the globals.
5. All resolution and enforcement stays in Rust, at dispatch.

## Security model

### Scopes

| Scope | Stored in | On/off | Raw policy |
|---|---|---|---|
| Connection (global) | `mcp.json`: `enabled`, `default_policy`, `tool_policies` | `enabled`, editable only in Connections | `tool_policies[t] ?? default_policy` |
| Chat | new `chats.tool_overrides` JSON column | per-server `off` (this chat only; the server stays connected) | `chat.policies[s][t] ?? global` |
| Agent | agent config JSON (`AgentServerAccess`) | server included + `tools` allow-list (existing) | `policies[t] ?? default_policy ?? global` (`default_policy = None` marks a legacy agent and keeps today's behavior) |

A run's scope is its agent if the chat has one, otherwise the chat itself. Scheduled runs
use their agent's scope.

### Resolution order

This is one pure function, unit-tested:

1. **Connection disabled**: nothing from that server is offered or callable, in any scope.
2. **Off in scope** (chat `off`, agent doesn't include the server, or the tool is outside
   the agent's allow-list): the tool is not offered and not named to the model.
3. **Global Block is a hard ceiling.** If `tool_policies[t] ?? default_policy == Block`, the
   result is Block in every scope. Scopes can only choose Allow or Ask, or turn tools off;
   they can tighten but never loosen a global Disable.
4. Otherwise the scope's raw policy applies. If it is `AlwaysAllow`, verify the signature
   pin of the scope that produced it: the scoped pin for a scope entry, the global pin for an
   inherited value. A mismatch or a missing pin gives Ask.

### Signature pins per scope

- `mcp.json` gains
  `scoped_signatures: HashMap<ScopeKey, HashMap<server, HashMap<tool, sig>>>`. Global pins
  stay in `signatures`.
- Re-granting in one scope never re-validates a stale grant in another.
- When a scope stores an `AlwaysAllow` (an edit, an agent snapshot, or *Always allow* on an
  approval card), Rust pins it straight away:
  - It copies the **global pin** when one exists, so a stale grant stays stale and a
    snapshot can't launder a changed tool into a fresh pin.
  - Otherwise it pins the live signature, or the cached one if the server is offline.
  - Scoped grants never pin on first use.

### Agent snapshot

- When the agent editor adds a server (or opens a legacy agent), it copies
  `default_policy` and each tool's current global policy.
- **Allow is copied only for `read_only` tools.** Write, destructive and unannotated tools
  start as Ask and must be granted to the agent explicitly. The reason: agents run
  unattended on schedules.
- The agent's *New tools* default is **Ask**. Choosing Allow shows an inline warning.
  Scheduled runs emit a Notice when a tool that wasn't present when the agent was saved is
  auto-allowed.
- The editor summarizes "N tools run without asking", with access badges.

### Dispatch-time re-checks

- `ScopedHost::policy` re-evaluates the scope's on/off state at dispatch. A server or tool
  turned off mid-run returns Block. Mid-run changes can only tighten the catalog; newly
  allowed tools wait for the next run.
- **Fail closed.** If the scope is missing (its chat or agent was deleted mid-run), every
  tool resolves to Ask; unattended runs auto-deny it. `delete_chat` and `delete_agent` also
  cancel active runs for that scope.

### Persistence and input validation

- Grants update memory first, so the run proceeds. If the DB write fails, the failure is
  logged and `mcp-changed` is emitted so the UI re-reads the persisted state. The next run
  then falls back to Ask.
- Overrides and agent access come from the webview, so they are validated:
  - `server_id` must be known; unknown servers are dropped.
  - Tool names are trimmed and limited to 128 chars.
  - At most 500 tools per server and 64 servers; larger input is rejected.
- `forget_scope` runs when a chat or agent is deleted. At startup, scopes whose chat or
  agent no longer exists are pruned.
- The approval button reads "Always allow in this chat" or "Always allow for this agent".

## Implementation plan

### agent-core

1. **`mcp/permissions.rs` (new)**:
   - `PermissionScope { Chat(ChatId), Agent(AgentId) }` and `ScopeKey`.
   - `ChatToolOverrides { servers: BTreeMap<server_id, ServerOverride { off, policies }> }`.
   - `ScopePolicies` (Chat / Agent / Missing), with `allows_server`, `allows_tool`, and
     `resolve(server, tool, global) -> Resolved { policy, pin_scope }` implementing the
     resolution order above.
   - Input validation and unit tests.
2. **`mcp/config.rs`**:
   - `scoped_signatures`.
   - `verify_allow(pin_scope, server, tool, sig)`, generalizing `effective_policy`.
   - `pin_scope_grants(scope, entries)` (copies the global pin, else live/cached).
   - `forget_scope`.
   - `prune_scopes(live_keys)`.
3. **`mcp/manager.rs`**:
   - Scope registry `scopes: Mutex<HashMap<ScopeKey, ScopePolicies>>` with `set_scope` and
     `remove_scope`.
   - `set_grant_hook`, which the Tauri layer uses to persist chat/agent grants.
   - `ScopedHost { mcp, scope }` implementing `ToolHost`, with the dispatch re-check and
     fail-closed behavior.
   - `prepare_tools(history, ctx, scope)` replaces the `ToolFilter` argument:
     - chat-off servers and agent-off tools are excluded;
     - Block is listed as disabled;
     - the namer still sees every tool, so exposed names stay stable.
   - `list_scoped(scope)` returns `Vec<ScopedServerView>`, where `ScopedServerView` is
     `{ server_id, in_scope, global_enabled, tools: [{ name, policy, overridden, ceiling_blocked }] }`.
     The UI renders any scope from this without re-implementing the resolution logic.
4. **`agents/mod.rs`**:
   - `AgentServerAccess` gains `default_policy: Option<ToolPolicy>` and
     `policies: BTreeMap<String, ToolPolicy>`. Both use serde defaults, so no migration is
     needed.
   - Add validation.
   - `permission_scope()` replaces `tool_filter()`.
5. **`storage/migrations.rs`**: append
   `ALTER TABLE chats ADD COLUMN tool_overrides TEXT;`.
   **`storage/chats.rs`**: get/set overrides (JSON; NULL means none).

### src-tauri

6. **`commands/mcp.rs`**: new commands:
   - `list_tool_permissions(scope)`
   - `set_chat_server_enabled(chat_id, server_id, on)`
   - `set_chat_tool_policy(chat_id, server_id, tool, policy: Option<ToolPolicy>)`, where
     `None` resets to the default
   - `reset_chat_tool_permissions(chat_id)`
   - `set_agent_server_access(agent_id, access | null)`

   Each one validates, writes to the DB, calls `set_scope` and then `pin_scope_grants`, and
   emits `mcp-changed` (plus `agents-changed` for agents).
7. **`commands/mod.rs::launch_run`**:
   - Build the scope and load the chat's overrides.
   - Call `set_scope`, then `prepare_tools(scope)`.
   - Use `ScopedHost` as the run's host.
8. **`state.rs` / `lib.rs`**: install the grant hook, register the new commands, and prune
   orphaned scopes at startup.
9. **`commands/chats.rs` / `commands/agents.rs`**:
   - Deleting a chat or agent cancels its active runs and calls `forget_scope`.
   - `save_agent` calls `pin_scope_grants`.

### Frontend

10. **`ToolsMenu.svelte`**:
    - Takes a `scope` prop. The header shows "This chat" or "Agent · name".
    - Each server toggle applies to that scope only. Servers disabled in Connections are
      greyed out, with "Off in Connections → Enable…".
    - Tool rows show the badge and `ToolPolicySelect` (Allow/Ask). Tools blocked by the
      ceiling show "disabled in Connections" instead.
    - An override dot plus "Reset to defaults".
    - In agent scope, rows also get an on/off checkbox, and changes are written via
      `setAgentServerAccess`.
    - **`Composer.svelte`** passes the scope, and `⚒ Tools · N` counts from the scoped view.
11. **`mcp-store.svelte.ts`**: scoped views cached per key and refetched on `mcp-changed`,
    plus wrappers for the new commands. The menu stops calling the global `setEnabled`.
12. **`AgentEditor.svelte`**, Tools card:
    - An include checkbox per server.
    - A *New tools* select (Off / Ask / Allow; default Ask; warns on Allow).
    - Per tool: an on/off toggle, Allow/Ask, and a badge.
    - Applies the snapshot rules above, with the "runs without asking" summary and
      "Reset to connection defaults".
    - Shows which tools are blocked by the ceiling and which servers are globally disabled.
13. **`ToolCallCard.svelte`**: the *Always allow* label names the scope.
14. Run `npm run gen:types` and update `src/lib/ipc/commands.ts`.

### Docs

15. `features.md`:
    - Rewrite *Tool security* to cover scopes, the ceiling, and per-scope pins.
    - Update the *Agents* section: permissions are now owned by the agent rather than
      "global policies still apply".
    - Add a `CHANGELOG.md` entry, and remove the README "Proposed features" row.

## Verification

- Unit tests:
  - Resolution: chat override; agent snapshot; agent new-tool fallback; legacy agent falls
    back to global; the ceiling; scope turned off at dispatch; missing scope gives Ask.
  - Per-scope pins: a re-grant in a chat doesn't re-validate the global grant; a snapshot
    copies a stale pin and stays Ask.
  - `prepare_tools` exclusion and disabled listing.
  - The migration and an overrides round trip.
  - Legacy `AgentServerAccess` JSON still deserializes.
  - Validation bounds.
- `npm run gen:types && npm run verify`.
- Manual, with `scripts/dev/mock-mcp-server.mjs` and `scripts/dev/mock-openai-server.mjs`:
  - Turning a server off in one chat keeps it connected for other chats.
  - "use tool echo" plus *Always allow* affects only that chat.
  - An agent created after setting defaults copies them (read-only tools keep Allow, the
    rest become Ask).
  - *Always allow* in an agent chat updates only the agent.
  - A globally disabled tool can't be enabled from a chat or an agent.
  - A scheduled run uses the agent's permissions.
  - Deleting the agent mid-run denies further calls.

## Risks and open points

- **Behavior change:** *Always allow* in a plain chat no longer applies globally; defaults
  live in Connections. The button label makes this explicit. Existing global
  `tool_policies` carry over as the defaults, so no data migration is needed.
- If an agent is edited from its chat's menu while the agent editor is open, the editor
  refreshes its Tools card on `agents-changed` as long as that card is untouched. If it has
  been edited, the next Save wins.
- Depends on PR A's cached signatures (`KnownTool`) and `ToolAccess`.
