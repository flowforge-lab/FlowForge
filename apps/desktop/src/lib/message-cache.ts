import type { Message } from "@/bindings";

const STORAGE_PREFIX = "ff-msg-cache:";
const MAX_CACHED_MESSAGES = 50;

/**
 * Read all cached session messages from localStorage. Returns a map that can
 * seed `messagesBySession` synchronously at store creation — eliminating the
 * cold-start empty-pane flash (#796).
 */
export function readCache(): Record<string, Message[]> {
  const result: Record<string, Message[]> = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key?.startsWith(STORAGE_PREFIX)) continue;
      const sessionId = key.slice(STORAGE_PREFIX.length);
      const raw = localStorage.getItem(key);
      if (raw) {
        result[sessionId] = JSON.parse(raw) as Message[];
      }
    }
  } catch {
    // Corrupted or unavailable localStorage — start empty, loadSession will hydrate.
  }
  return result;
}

/**
 * Persist the most recent messages for a session. Capped to avoid bloating
 * localStorage; loadSession() overwrites with full backend truth anyway.
 */
export function writeCache(sessionId: string, messages: Message[]): void {
  try {
    const capped = messages.slice(-MAX_CACHED_MESSAGES);
    localStorage.setItem(STORAGE_PREFIX + sessionId, JSON.stringify(capped));
  } catch {
    // Quota exceeded or private mode — non-fatal.
  }
}

/** Remove the cache entry for a deleted session. */
export function clearCache(sessionId: string): void {
  try {
    localStorage.removeItem(STORAGE_PREFIX + sessionId);
  } catch {
    // Non-fatal.
  }
}
