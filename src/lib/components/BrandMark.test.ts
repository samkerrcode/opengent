import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import BrandMark from "./BrandMark.svelte";

describe("BrandMark", () => {
  it("uses the shared SVG as a decorative, theme-colored mask", () => {
    const { body } = render(BrandMark);
    expect(body).toContain("logo.svg");
    expect(body).toContain('aria-hidden="true"');
    expect(body).toContain("width: 24px");
    expect(body).toContain("height: 24px");
  });

  it("supports larger welcome-screen marks without duplicating geometry", () => {
    const { body } = render(BrandMark, { props: { size: 48 } });
    expect(body).toContain("width: 48px");
    expect(body).toContain("height: 48px");
    expect(body).not.toContain("<path");
  });
});
