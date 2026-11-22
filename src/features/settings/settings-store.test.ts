import { describe, expect, it } from "vitest";
import { applyPatch } from "./settings-store.svelte";
import type { Settings } from "$lib/ipc/commands";

describe("applyPatch", () => {
  // A second field in each section shows the merge keeps fields it isn't given.
  const base = { general: { share_environment: true }, appearance: { theme: "system", font_size: 14 } } as unknown as Settings;

  it("replaces only the named fields and leaves the input untouched", () => {
    const next = applyPatch(base, { appearance: { theme: "dark" } }) as unknown as Record<string, Record<string, unknown>>;
    expect(next.appearance).toEqual({ theme: "dark", font_size: 14 });
    expect(next.general).toBe((base as unknown as Record<string, object>).general);
    expect((base as unknown as Record<string, Record<string, unknown>>).appearance!.theme).toBe("system");
  });

  it("sets the theme the Appearance pane sends", () => {
    const settings: Settings = { general: { share_environment: true }, appearance: { theme: "system" } };
    expect(applyPatch(settings, { appearance: { theme: "light" } }).appearance.theme).toBe("light");
    expect(settings.appearance.theme).toBe("system");
  });
});
