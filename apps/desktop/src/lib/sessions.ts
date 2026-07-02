// Pure helpers for the session list (Issue #19). Kept free of React so the label
// resolution + filter are unit-testable and shared by the sidebar's display and
// its search — so what you type matches exactly what you see.

import type { Session } from "@/bindings";

/**
 * The label a user sees for a session: persisted title > goal > fallback.
 * `session.title` is server-truth (auto-derived from the first user message,
 * overridable via `rename_session`); the durable session store (ff-session,
 * RFC 0012) means the title survives a restart, so the frontend no longer
 * keeps a legacy localStorage fallback (#52).
 */
export function resolveLabel(session: Session): string {
  if (session.title) return session.title;
  if (session.goal) return session.goal;
  return "New session";
}

/**
 * Client-side filter: case-insensitive substring over the *resolved* label, so a
 * renamed title and a goal both match what the user actually sees. An empty or
 * whitespace-only query returns the list unchanged, order preserved.
 */
export function filterSessions(sessions: Session[], query: string): Session[] {
  const q = query.trim().toLowerCase();
  if (!q) return sessions;
  return sessions.filter((session) =>
    resolveLabel(session).toLowerCase().includes(q),
  );
}

/** How many rows the list grows by per "Show more" click, and the initial
 *  batch size (#667). One endless, incremental reveal replaced the old
 *  All/Dismissed tabs + fixed cap. */
export const SESSION_REVEAL_BATCH = 25;

/**
 * Order the full session list for display (#667): pinned first, then the rest of
 * the non-dismissed sessions, then dismissed sessions last. Order within each of
 * the three groups is preserved (stable). Dismissed always sink to the bottom
 * regardless of pin, so a dismissed session never floats above a live one.
 */
export function arrangeSessions(
  sessions: Session[],
  pinned: ReadonlySet<string>,
  dismissed: ReadonlySet<string>,
): Session[] {
  const rank = (s: Session): number => {
    if (dismissed.has(s.id)) return 2; // dismissed: always last
    if (pinned.has(s.id)) return 0; // pinned live: first
    return 1; // other live
  };
  // Stable sort by group rank (Array.prototype.sort is stable), so insertion
  // order is kept within each group.
  return [...sessions].sort((a, b) => rank(a) - rank(b));
}

/**
 * Reveal the first `revealCount` of the arranged list, always keeping the active
 * session visible even if it falls past the cut (#185 carried forward). The
 * caller arranges first (pinned → non-dismissed → dismissed) and filters before
 * this, so batching applies to exactly what the user is looking at (#667).
 *
 * `hasMore` is true when arranged rows remain beyond what's shown, driving the
 * "Show more" affordance. A pure view-state helper.
 */
export function selectSessionOverflow(
  arranged: Session[],
  activeSessionId: string | null,
  revealCount: number,
): { visible: Session[]; hasMore: boolean } {
  if (revealCount >= arranged.length) {
    return { visible: arranged, hasMore: false };
  }
  const head = arranged.slice(0, revealCount);
  // Pull the active session in even when it sits past the reveal cut, so the
  // current session is never hidden behind "Show more".
  if (activeSessionId && !head.some((s) => s.id === activeSessionId)) {
    const active = arranged.find((s) => s.id === activeSessionId);
    if (active) head.push(active);
  }
  return { visible: head, hasMore: true };
}
