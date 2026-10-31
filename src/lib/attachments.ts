// Memoized `data:` URLs for image attachments. Blobs are immutable (content-addressed), so a
// fetched URL never goes stale for a given id.
import { commands } from "$lib/ipc/commands";

const cache = new Map<string, Promise<string | null>>();

export function attachmentDataUrl(id: string): Promise<string | null> {
  let p = cache.get(id);
  if (!p) {
    p = commands.attachmentDataUrl(id).catch(() => null);
    cache.set(id, p);
  }
  return p;
}
