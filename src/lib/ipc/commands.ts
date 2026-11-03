// Typed wrappers over Tauri commands. Types come from ts-rs bindings generated from Rust
// (`npm run gen:types`), so the IPC contract has exactly one source of truth.
import { invoke, Channel } from "@tauri-apps/api/core";
import type { ActiveSelection } from "$lib/generated/bindings/ActiveSelection";
import type { AppInfo } from "$lib/generated/bindings/AppInfo";
import type { AttachmentMeta } from "$lib/generated/bindings/AttachmentMeta";
import type { Capabilities } from "$lib/generated/bindings/Capabilities";
import type { ChatDetail } from "$lib/generated/bindings/ChatDetail";
import type { ChatSummary } from "$lib/generated/bindings/ChatSummary";
import type { ChatId } from "$lib/generated/bindings/ChatId";
import type { Error as CoreError } from "$lib/generated/bindings/Error";
import type { ImageSupport } from "$lib/generated/bindings/ImageSupport";
import type { ProviderConfig } from "$lib/generated/bindings/ProviderConfig";
import type { ProviderInput } from "$lib/generated/bindings/ProviderInput";
import type { ProviderTestResult } from "$lib/generated/bindings/ProviderTestResult";
import type { RunEvent } from "$lib/generated/bindings/RunEvent";
import type { RunId } from "$lib/generated/bindings/RunId";
import type { RunStarted } from "$lib/generated/bindings/RunStarted";
import type { StoredMessage } from "$lib/generated/bindings/StoredMessage";
import type { SecretProbe } from "$lib/generated/bindings/SecretProbe";
import type { TlsMode } from "$lib/generated/bindings/TlsMode";
import type { ApprovalDecision } from "$lib/generated/bindings/ApprovalDecision";
import type { KeyValueInput } from "$lib/generated/bindings/KeyValueInput";
import type { McpServerInput } from "$lib/generated/bindings/McpServerInput";
import type { McpServerView } from "$lib/generated/bindings/McpServerView";
import type { McpTransport } from "$lib/generated/bindings/McpTransport";
import type { ToolAccess } from "$lib/generated/bindings/ToolAccess";
import type { ToolCallInfo } from "$lib/generated/bindings/ToolCallInfo";
import type { ToolCallStatus } from "$lib/generated/bindings/ToolCallStatus";
import type { ToolInfo } from "$lib/generated/bindings/ToolInfo";
import type { ToolLoading } from "$lib/generated/bindings/ToolLoading";
import type { ToolPolicy } from "$lib/generated/bindings/ToolPolicy";
import type { ToolSettings } from "$lib/generated/bindings/ToolSettings";
import type { Agent } from "$lib/generated/bindings/Agent";
import type { AgentInput } from "$lib/generated/bindings/AgentInput";
import type { AgentSchedule } from "$lib/generated/bindings/AgentSchedule";
import type { AgentServerAccess } from "$lib/generated/bindings/AgentServerAccess";
import type { MissedRunPolicy } from "$lib/generated/bindings/MissedRunPolicy";
import type { PickedAttachments } from "$lib/generated/bindings/PickedAttachments";
import type { ModelRef } from "$lib/generated/bindings/ModelRef";
import type { Recurrence } from "$lib/generated/bindings/Recurrence";
import type { ScheduleInput } from "$lib/generated/bindings/ScheduleInput";
import type { ScheduleOutcome } from "$lib/generated/bindings/ScheduleOutcome";

export type {
  Agent,
  AgentInput,
  AgentSchedule,
  AgentServerAccess,
  MissedRunPolicy,
  PickedAttachments,
  ModelRef,
  Recurrence,
  ScheduleInput,
  ScheduleOutcome,
  ActiveSelection,
  AppInfo,
  ApprovalDecision,
  AttachmentMeta,
  Capabilities,
  ChatDetail,
  ChatId,
  ChatSummary,
  CoreError,
  ImageSupport,
  KeyValueInput,
  McpServerInput,
  McpServerView,
  McpTransport,
  ProviderConfig,
  ProviderInput,
  ProviderTestResult,
  RunEvent,
  RunId,
  RunStarted,
  SecretProbe,
  StoredMessage,
  TlsMode,
  ToolAccess,
  ToolCallInfo,
  ToolCallStatus,
  ToolInfo,
  ToolLoading,
  ToolPolicy,
  ToolSettings,
};

export function isCoreError(value: unknown): value is CoreError {
  return typeof value === "object" && value !== null && "kind" in value;
}

export function describeError(error: unknown): string {
  if (isCoreError(error)) {
    return "message" in error ? error.message : error.kind.replace(/_/g, " ");
  }
  return error instanceof Error ? error.message : String(error);
}

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  /** `providerId`/`model` are ignored (pass null) for agent chats: Rust uses the agent's. */
  startRun(
    chatId: ChatId,
    providerId: string | null,
    model: string | null,
    prompt: string,
    attachmentIds: string[],
    replaceFrom: string | null,
    onEvent: (event: RunEvent) => void,
  ): Promise<RunStarted> {
    const channel = new Channel<RunEvent>();
    channel.onmessage = onEvent;
    return invoke<RunStarted>("start_run", { chatId, providerId, model, prompt, attachmentIds, replaceFrom, onEvent: channel });
  },

  listChats: () => invoke<ChatSummary[]>("list_chats"),
  searchChats: (query: string) => invoke<ChatSummary[]>("search_chats", { query }),
  createChat: (agentId: string | null = null) => invoke<ChatSummary>("create_chat", { agentId }),
  getChat: (id: ChatId) => invoke<ChatDetail>("get_chat", { id }),
  renameChat: (id: ChatId, title: string) => invoke<void>("rename_chat", { id, title }),
  deleteChat: (id: ChatId) => invoke<void>("delete_chat", { id }),
  pickAttachments: (includeImages: boolean) =>
    invoke<PickedAttachments>("pick_attachments", { includeImages }),
  attachmentDataUrl: (attachmentId: string) =>
    invoke<string>("attachment_data_url", { attachmentId }),

  cancelRun: (runId: RunId) => invoke<boolean>("cancel_run", { runId }),

  probeSecretStore: () => invoke<SecretProbe>("probe_secret_store"),

  listProviders: () => invoke<ProviderConfig[]>("list_providers"),
  saveProvider: (input: ProviderInput) => invoke<ProviderConfig>("save_provider", { input }),
  deleteProvider: (id: string) => invoke<void>("delete_provider", { id }),
  testProvider: (id: string) => invoke<ProviderTestResult>("test_provider", { id }),
  getModelCapabilities: (providerId: string, model: string) =>
    invoke<Capabilities>("get_model_capabilities", { providerId, model }),
  getActiveSelection: () => invoke<ActiveSelection | null>("get_active_selection"),
  setActiveSelection: (selection: ActiveSelection) => invoke<void>("set_active_selection", { selection }),

  // MCP connections & tool security (Phase 3).
  listMcpServers: () => invoke<McpServerView[]>("list_mcp_servers"),
  /** Returns the server id. `connect` forces a (re)connect ("Save & test"); otherwise only
   * changes to how the server is reached reconnect it. Disabled servers never connect. */
  saveMcpServer: (input: McpServerInput, connect: boolean) =>
    invoke<string>("save_mcp_server", { input, connect }),
  deleteMcpServer: (id: string) => invoke<void>("delete_mcp_server", { id }),
  setMcpServerEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_mcp_server_enabled", { id, enabled }),
  reconnectMcpServer: (id: string) => invoke<void>("reconnect_mcp_server", { id }),
  setMcpToolPolicy: (serverId: string, tool: string, policy: ToolPolicy) =>
    invoke<void>("set_mcp_tool_policy", { serverId, tool, policy }),
  setMcpDefaultPolicy: (serverId: string, policy: ToolPolicy) =>
    invoke<void>("set_mcp_default_policy", { serverId, policy }),
  resetMcpToolPolicies: (serverId: string) => invoke<void>("reset_mcp_tool_policies", { serverId }),
  getToolSettings: () => invoke<ToolSettings>("get_tool_settings"),
  setToolSettings: (settings: ToolSettings) => invoke<void>("set_tool_settings", { settings }),
  resolveToolApproval: (runId: RunId, callId: string, decision: ApprovalDecision) =>
    invoke<boolean>("resolve_tool_approval", { runId, callId, decision }),

  // User-configured agents and schedules.
  listAgents: () => invoke<Agent[]>("list_agents"),
  saveAgent: (input: AgentInput) => invoke<Agent>("save_agent", { input }),
  deleteAgent: (id: string) => invoke<void>("delete_agent", { id }),
  listSchedules: (agentId: string | null = null) => invoke<AgentSchedule[]>("list_schedules", { agentId }),
  saveSchedule: (input: ScheduleInput) => invoke<AgentSchedule>("save_schedule", { input }),
  setScheduleEnabled: (id: string, enabled: boolean) =>
    invoke<AgentSchedule>("set_schedule_enabled", { id, enabled }),
  deleteSchedule: (id: string) => invoke<void>("delete_schedule", { id }),
  runScheduleNow: (id: string) => invoke<ChatSummary>("run_schedule_now", { id }),
};
