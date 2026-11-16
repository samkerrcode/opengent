// Attachment helpers for the UI: what a file is, whether the current model can be sent it,
// and memoized `data:` URLs for thumbnails. Blobs are immutable (content-addressed), so a
// fetched URL never goes stale for a given id.
import { commands } from "$lib/ipc/commands";

export type AttachmentKind = "text" | "image" | "pdf";

/** How an attachment reaches the model. Mirrors `AttachmentMeta::kind` in Rust. */
export function attachmentKind(a: { mime: string }): AttachmentKind {
  if (a.mime === "application/pdf") return "pdf";
  return a.mime.startsWith("image/") ? "image" : "text";
}

/** Why the model can't be sent this attachment, or null if it can. Mirrors
 * `AttachmentMeta::unusable_reason` in Rust, which is what actually gates a send; both
 * are pinned to `attachment-reasons.json` by their tests. */
export function blockedReason(a: { mime: string }, caps: { images: boolean }): string | null {
  if (caps.images) return null;
  switch (attachmentKind(a)) {
    case "image":
      return "the selected model does not accept image attachments";
    case "pdf":
      return "the selected model does not accept images, and PDFs are sent as page images";
    default:
      return null;
  }
}

/** Chip label for a PDF: `PDF · 12 pages`, or `PDF · pages 1–20 of 57` when only the
 * leading `shown` pages are sent. `shown` of 0 means "not known" (e.g. a message sent
 * before the count was recorded). */
export function pdfLabel(pageCount: number | null | undefined, shown: number | null = 0): string {
  if (!pageCount) return "PDF";
  if (shown && shown < pageCount) return `PDF · pages 1–${shown} of ${pageCount}`;
  return `PDF · ${pageCount} ${pageCount === 1 ? "page" : "pages"}`;
}

const cache = new Map<string, Promise<string | null>>();

/** Thumbnail source for an image attachment, or a small image of a PDF's page 1. */
export function attachmentDataUrl(id: string): Promise<string | null> {
  let p = cache.get(id);
  if (!p) {
    p = commands.attachmentDataUrl(id).catch(() => null);
    cache.set(id, p);
  }
  return p;
}

/** Composer status while picked or dropped PDFs are rendered. */
export function importLabel(p: { name: string | null; done: number; total: number }): string | null {
  if (p.name === null) return null;
  return p.total > 1 ? `Rendering ${p.name} (${p.done + 1} of ${p.total})…` : `Rendering ${p.name}…`;
}
