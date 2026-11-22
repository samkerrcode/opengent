// `get_app_info` never changes while the app runs, so it is fetched once and shared.
import { commands, type AppInfo } from "$lib/ipc/commands";

let cached: Promise<AppInfo> | null = null;

export function loadAppInfo(): Promise<AppInfo> {
  return (cached ??= commands.getAppInfo().catch((e: unknown) => {
    cached = null;
    throw e;
  }));
}
