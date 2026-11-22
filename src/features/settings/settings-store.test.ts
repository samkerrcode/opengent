import { describe, expect, it } from "vitest";
import { applyPatch } from "./settings-store.svelte";
import type { Settings } from "$lib/ipc/commands";

describe("applyPatch", () => {
  // Sections don't have fields yet; cast a richer shape to exercise the merge.
  const base = { general: {}, appearance: { theme: "system", font_size: 14 } } as unknown as Settings;

  it("replaces only the named fields and leaves the input untouched", () => {
    const next = applyPatch(base, { appearance: { theme: "dark" } } as never) as unknown as Record<string, Record<string, unknown>>;
    expect(next.appearance).toEqual({ theme: "dark", font_size: 14 });
    expect(next.general).toBe((base as unknown as Record<string, object>).general);
    expect((base as unknown as Record<string, Record<string, unknown>>).appearance!.theme).toBe("system");
  });
});
