// Typed wrappers over Tauri commands. Types come from ts-rs bindings generated from Rust
// (`npm run gen:types`), so the IPC contract has exactly one source of truth.
import { invoke, Channel } from "@tauri-apps/api/core";
import type { AppInfo } from "$lib/generated/bindings/AppInfo";
import type { ChatId } from "$lib/generated/bindings/ChatId";
import type { Error as CoreError } from "$lib/generated/bindings/Error";
import type { RunEvent } from "$lib/generated/bindings/RunEvent";
import type { RunId } from "$lib/generated/bindings/RunId";
import type { SecretProbe } from "$lib/generated/bindings/SecretProbe";

export type { AppInfo, ChatId, CoreError, RunEvent, RunId, SecretProbe };

export function isCoreError(value: unknown): value is CoreError {
  return typeof value === "object" && value !== null && "kind" in value;
}

export function describeError(error: unknown): string {
  if (isCoreError(error)) {
    return "message" in error ? `${error.kind.replace(/_/g, " ")}: ${error.message}` : error.kind.replace(/_/g, " ");
  }
  return error instanceof Error ? error.message : String(error);
}

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  startRun(chatId: ChatId, prompt: string, onEvent: (event: RunEvent) => void): Promise<RunId> {
    const channel = new Channel<RunEvent>();
    channel.onmessage = onEvent;
    return invoke<RunId>("start_run", { chatId, prompt, onEvent: channel });
  },

  cancelRun: (runId: RunId) => invoke<boolean>("cancel_run", { runId }),

  probeSecretStore: () => invoke<SecretProbe>("probe_secret_store"),
};
