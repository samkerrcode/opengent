// Synchronous platform check for shortcut handling and hints, before `get_app_info` returns.
export const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

/** Prefix for shortcut hints: `⌘N` on macOS, `Ctrl+N` elsewhere. */
export const modKey = isMac ? "⌘" : "Ctrl+";
