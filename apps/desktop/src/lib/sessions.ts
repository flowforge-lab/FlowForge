// Pure helpers for the session list (Issue #19). Kept free of React so the label
// resolution + filter are unit-testable and shared by the sidebar's display and
// its search — so what you type matches exactly what you see.

import type { Session } from "@/bindings";
import type { SearchHit } from "@/bindings/SearchHit";

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

const FORK_SUFFIX_RE = / \(Fork (\d+)\)$/;

/** Strip a trailing " (Fork <k>)" suffix (#1069) so forking a fork renumbers
 *  from the original base title instead of stacking suffixes. */
export function stripForkSuffix(title: string): string {
  return title.replace(FORK_SUFFIX_RE, "");
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Compute the next "<base> (Fork N)" title for a session forked from
 * `sourceTitle` (#1069). `base` strips any existing "(Fork k)" suffix from the
 * source so forking a fork stays on the same base and keeps numbering
 * contiguous; `N` is one past the highest existing "(Fork N)" sharing that
 * base among `existingTitles` (the in-memory session list), or 1 if none.
 */
export function nextForkTitle(
  sourceTitle: string,
  existingTitles: (string | null)[],
): string {
  const base = stripForkSuffix(sourceTitle);
  const re = new RegExp(`^${escapeRegExp(base)} \\(Fork (\\d+)\\)$`);
  let max = 0;
  for (const title of existingTitles) {
    if (!title) continue;
    const m = re.exec(title);
    if (m) max = Math.max(max, Number(m[1]));
  }
  return `${base} (Fork ${max + 1})`;
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

/**
 * One content-search result to show as its own sidebar row (#710): the matched
 * session paired with its best hit (for the snippet + jump-to-message).
 */
export interface ContentHitRow {
  session: Session;
  hit: SearchHit;
}

/**
 * Reduce cross-session `searchMessages` hits (BM25 order, possibly several per
 * session) to one row per session for the sidebar's content group (#710):
 * - keep the first (best-ranked) hit per session, preserving BM25 order;
 * - drop sessions already shown as title matches (`excludeSessionIds`);
 * - drop hits whose session isn't in `sessionsById` (e.g. a not-yet-listed
 *   draft), so every row resolves to a real, visible session.
 */
export function groupContentHits(
  hits: SearchHit[],
  excludeSessionIds: ReadonlySet<string>,
  sessionsById: ReadonlyMap<string, Session>,
): ContentHitRow[] {
  const seen = new Set<string>();
  const rows: ContentHitRow[] = [];
  for (const hit of hits) {
    if (seen.has(hit.sessionId) || excludeSessionIds.has(hit.sessionId)) {
      continue;
    }
    const session = sessionsById.get(hit.sessionId);
    if (!session) continue;
    seen.add(hit.sessionId);
    rows.push({ session, hit });
  }
  return rows;
}

/**
 * Make an FTS5 `snippet()` string safe to inject as HTML (#710, PR #747 C1).
 * The backend wraps matched terms in `<mark>` but does NOT escape the
 * surrounding message text, which routinely contains raw HTML/JS (agents quote
 * `<script>`, `<img onerror=…>`, etc.). Rendering it verbatim in the Tauri
 * webview would be a live XSS with full IPC access. So we escape every text
 * segment and re-emit only the backend's own `<mark>`/`</mark>` delimiters.
 */
export function sanitizeSnippet(raw: string): string {
  return raw
    .split(/(<\/?mark>)/g)
    .map((part) =>
      part === "<mark>" || part === "</mark>" ? part : escapeHtml(part),
    )
    .join("");
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/**
 * A short, human relative date for a search-hit row (#876): "Today"/"Yesterday"
 * for the last two calendar days (by local wall-clock day, not a rolling
 * 24h/48h window), else a short date ("Jul 3"), with the year appended only
 * when it differs from the current year ("Jul 3, 2024"). Distinct from
 * memory-view.ts's `humanAge()` ("~3 days ago" style), which the mockups don't
 * use. `Date.UTC` on each date's local Y/M/D is just an integer-day bucketing
 * trick to avoid DST arithmetic bugs from a naive ms-diff divide.
 */
export function formatHitDate(
  createdAtMs: number,
  now: number = Date.now(),
): string {
  const day = (ms: number) => {
    const d = new Date(ms);
    return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate());
  };
  const diffDays = Math.round((day(now) - day(createdAtMs)) / 86_400_000);
  if (diffDays === 0) return "Today";
  if (diffDays === 1) return "Yesterday";
  const created = new Date(createdAtMs);
  const sameYear = created.getFullYear() === new Date(now).getFullYear();
  return created.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    year: sameYear ? undefined : "numeric",
  });
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
  // `hasMore` only when rows still remain hidden after the pulled-in active — so
  // pulling in the single overflow row (active) doesn't strand a "Show more" that
  // reveals nothing.
  return { visible: head, hasMore: head.length < arranged.length };
}
