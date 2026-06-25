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

/** Max unpinned sessions shown before the overflow affordance (#185). */
export const UNPINNED_SESSION_CAP = 15;

export type SessionListTab = "all" | "dismissed";

/**
 * Apply the sidebar's view preferences (#169, #185): All tab hides dismissed;
 * Dismissed tab shows only dismissed. Pinned sessions float to the top. Order
 * within each group is preserved (stable).
 */
export function arrangeSessions(
  sessions: Session[],
  pinned: ReadonlySet<string>,
  dismissed: ReadonlySet<string>,
  tab: SessionListTab,
): Session[] {
  return sessions
    .filter((s) => (tab === "all" ? !dismissed.has(s.id) : dismissed.has(s.id)))
    .sort((a, b) => Number(pinned.has(b.id)) - Number(pinned.has(a.id)));
}

/**
 * Cap unpinned sessions at {@link UNPINNED_SESSION_CAP} while always showing
 * every pinned session and the active session (#185). Pure view-state helper.
 */
export function selectSessionOverflow(
  sessions: Session[],
  pinned: ReadonlySet<string>,
  activeSessionId: string | null,
  revealAll: boolean,
  cap: number = UNPINNED_SESSION_CAP,
): { visible: Session[]; hiddenCount: number } {
  if (revealAll) return { visible: sessions, hiddenCount: 0 };

  const visible: Session[] = [];
  let unpinnedShown = 0;
  let unpinnedHidden = 0;

  for (const s of sessions) {
    const isPinned = pinned.has(s.id);
    const isActive = s.id === activeSessionId;
    if (isPinned || isActive) {
      visible.push(s);
      continue;
    }
    if (unpinnedShown < cap) {
      visible.push(s);
      unpinnedShown += 1;
    } else {
      unpinnedHidden += 1;
    }
  }

  return { visible, hiddenCount: unpinnedHidden };
}
