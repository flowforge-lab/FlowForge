// Short local clock time for a chat message's author header (#641). Kept free of
// React/stores so it's unit-testable in vitest's node env (mirrors lib/mcp.ts).

/**
 * Format a message's `createdAt` (Unix ms) as a short local clock time, e.g.
 * `07:31 AM`. Returns "" for a missing/zero/sentinel timestamp, matching the
 * "no timing" treatment in turn-groups' `validTs` (`ts > 0` is real backend epoch
 * ms) so the header omits the time rather than rendering a bogus value.
 */
export function formatMessageTime(createdAt: number): string {
  if (!(typeof createdAt === "number" && createdAt > 0)) return "";
  return new Date(createdAt).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}
