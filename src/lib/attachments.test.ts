import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { attachmentKind, blockedReason, importLabel, pdfLabel } from "./attachments";

// Shared with the Rust test `reasons_match_the_ui`, so the two gates can't drift apart.
const reasons = JSON.parse(readFileSync(new URL("./attachment-reasons.json", import.meta.url), "utf8")) as Record<
  string,
  string
>;

const png = { mime: "image/png" };
const pdf = { mime: "application/pdf" };
const source = { mime: "text/x-source" };
const json = { mime: "application/json" };

describe("attachmentKind", () => {
  it("classifies by mime", () => {
    expect(attachmentKind(png)).toBe("image");
    expect(attachmentKind({ mime: "image/webp" })).toBe("image");
    expect(attachmentKind(pdf)).toBe("pdf");
    expect(attachmentKind(source)).toBe("text");
    expect(attachmentKind(json)).toBe("text");
  });
});

describe("blockedReason", () => {
  it("allows everything on a vision model", () => {
    for (const a of [png, pdf, source, json]) expect(blockedReason(a, { images: true })).toBeNull();
  });

  it("blocks images and PDFs, but not text, on a text-only model", () => {
    expect(blockedReason(source, { images: false })).toBeNull();
    expect(blockedReason(json, { images: false })).toBeNull();
    expect(blockedReason(png, { images: false })).toBe(reasons.image);
    expect(blockedReason(pdf, { images: false })).toBe(reasons.pdf);
    expect(Object.keys(reasons).sort()).toEqual(["image", "pdf"]);
  });
});

describe("pdfLabel", () => {
  it("shows the page count", () => {
    expect(pdfLabel(12, 12)).toBe("PDF · 12 pages");
    expect(pdfLabel(1, 1)).toBe("PDF · 1 page");
  });

  it("discloses when only the leading pages are sent", () => {
    expect(pdfLabel(57, 20)).toBe("PDF · pages 1–20 of 57");
  });

  it("falls back when counts are unknown", () => {
    expect(pdfLabel(57)).toBe("PDF · 57 pages");
    expect(pdfLabel(null)).toBe("PDF");
    expect(pdfLabel(undefined, 3)).toBe("PDF");
    expect(pdfLabel(57, null)).toBe("PDF · 57 pages");
  });
});

describe("importLabel", () => {
  it("names the file being rendered and clears when done", () => {
    expect(importLabel({ name: "a.pdf", done: 0, total: 1 })).toBe("Rendering a.pdf…");
    expect(importLabel({ name: "b.pdf", done: 1, total: 3 })).toBe("Rendering b.pdf (2 of 3)…");
    expect(importLabel({ name: null, done: 3, total: 3 })).toBeNull();
  });
});
