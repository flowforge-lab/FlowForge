// Short local timestamp for a chat message's author header (#641, dated in
// #1259). Kept free of React/stores so it's unit-testable in vitest's node env
// (mirrors lib/mcp.ts) — `sessions.ts` is pure helpers with type-only imports,
// so composing on it preserves that.

import { formatHitDate } from "@/lib/sessions";

/**
 * Format a message's `createdAt` (Unix ms) for the header. Today's messages are
 * time-only (`07:31 AM`); anything older is prefixed with the day it landed on
 * (`Yesterday, 07:31 AM`, `Jul 3, 07:31 AM`, `Jul 3, 2024, 07:31 AM`) so a reply
 * from five minutes ago can't be mistaken for one from three days back (#1259).
 *
 * The day bucket comes from `formatHitDate` (#876) rather than a second copy of
 * the boundary logic: it splits on the local calendar day (not a rolling 24h
 * window) and appends the year only when it differs from `now`. Its "Today"
 * label is deliberately dropped instead of printed, since today's messages show
 * no date at all.
 *
 * Returns "" for a missing/zero/sentinel timestamp, matching the "no timing"
 * treatment in turn-groups' `validTs` (`ts > 0` is real backend epoch ms) so the
 * header omits the timestamp rather than rendering a bogus value.
 *
 * `now` is injectable so tests are deterministic (as in `formatHitDate`).
 */
export function formatMessageTime(
  createdAt: number,
  now: number = Date.now(),
): string {
  if (!(typeof createdAt === "number" && createdAt > 0)) return "";
  const time = new Date(createdAt).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  const day = formatHitDate(createdAt, now);
  return day === "Today" ? time : `${day}, ${time}`;
}
