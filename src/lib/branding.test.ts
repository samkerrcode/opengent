import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const { JSDOM } = createRequire(import.meta.url)("jsdom") as {
  JSDOM: new (source: string, options: { contentType: string }) => { window: { document: Document } };
};
const root = new URL("../../", import.meta.url);
const read = (path: string) => readFileSync(new URL(path, root));
const parse = (path: string) => new JSDOM(read(path).toString(), { contentType: "image/svg+xml" }).window.document;

describe("brand assets", () => {
  it("keeps the square master transparent and its terminal solid", () => {
    const doc = parse("docs/assets/logo.svg");
    expect(doc.documentElement.getAttribute("viewBox")).toBe("0 0 256 256");
    expect(doc.querySelector("rect, image")).toBeNull();
    expect(doc.querySelectorAll("path")).toHaveLength(3);
    expect(doc.querySelector("path[fill]")?.getAttribute("fill")).toBe("#2f6fed");
  });

  it.each(["logo-horizontal.svg", "logo-horizontal-dark.svg", "social-preview.svg"])(
    "%s is self-contained, outlined, and named from product identity",
    (file) => {
      const doc = parse(`docs/assets/${file}`);
      const product = JSON.parse(read("product.json").toString()) as { displayName: string };
      expect(doc.querySelector("title")?.textContent).toContain(product.displayName);
      expect(doc.querySelector("text, image, script, foreignObject")).toBeNull();
      expect(doc.querySelectorAll("path").length).toBeGreaterThan(3);
    },
  );

  it("shares the master geometry with the desktop icon", () => {
    const master = parse("docs/assets/logo.svg");
    const icon = parse("src-tauri/icons/source.svg");
    expect([...icon.querySelectorAll("path")].map((p) => p.getAttribute("d")))
      .toEqual([...master.querySelectorAll("path")].map((p) => p.getAttribute("d")));
    expect(icon.documentElement.getAttribute("viewBox")).toBe("0 0 1024 1024");
  });

  it("provides a 1280×640 social preview under 1 MB", () => {
    const png = read("docs/assets/social-preview.png");
    expect(png.subarray(0, 8).toString("hex")).toBe("89504e470d0a1a0a");
    expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1280, 640]);
    expect(png.length).toBeLessThan(1_000_000);
  });

  it("provides square desktop PNGs and native icon containers", () => {
    const files = readdirSync(fileURLToPath(new URL("src-tauri/icons/", root)));
    for (const file of files.filter((f) => f.endsWith(".png"))) {
      const png = read(`src-tauri/icons/${file}`);
      expect(png.subarray(0, 8).toString("hex")).toBe("89504e470d0a1a0a");
      expect(png.readUInt32BE(16)).toBe(png.readUInt32BE(20));
    }
    expect(read("src-tauri/icons/icon.icns").subarray(0, 4).toString()).toBe("icns");
    expect(read("src-tauri/icons/icon.ico").subarray(0, 4).toString("hex")).toBe("00000100");
  });
});
