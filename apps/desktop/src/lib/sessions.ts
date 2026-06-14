// Pure helpers for the session list (Issue #19). Kept free of React so the label
// resolution + filter are unit-testable and shared by the sidebar's display and
// its search — so what you type matches exactly what you see.

import type { Session } from "@/bindings";

/**
 * The label a user sees for a session: persisted title > legacy localStorage
 * title > goal > fallback. `session.title` is server-truth (auto-derived or
 * renamed); `customTitle` is the legacy `sessionTitles` map, kept as a
 * read-through fallback until every client has migrated (see store `bootstrap`).
 */
export function resolveLabel(
  session: Session,
  customTitle: string | undefined,
): string {
  if (session.title) return session.title;
  if (customTitle) return customTitle;
  if (session.goal) return session.goal;
  return "New session";
}

/**
 * Client-side filter: case-insensitive substring over the *resolved* label, so a
 * renamed title and a goal both match what the user actually sees. An empty or
 * whitespace-only query returns the list unchanged, order preserved.
 */
export function filterSessions(
  sessions: Session[],
  query: string,
  sessionTitles: Record<string, string>,
): Session[] {
  const q = query.trim().toLowerCase();
  if (!q) return sessions;
  return sessions.filter((session) =>
    resolveLabel(session, sessionTitles[session.id]).toLowerCase().includes(q),
  );
}
