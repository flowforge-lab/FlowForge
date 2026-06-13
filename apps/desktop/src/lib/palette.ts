// Pure helpers for the ⌘K command palette (Issue #16). Kept free of React and the
// zustand stores so the fuzzy matcher and registry builder are unit-testable in
// isolation — palette.tsx can't be imported under vitest's node env because
// store/palette.ts touches localStorage at module load. Mirrors lib/steps.ts and
// lib/sessions.ts.

import type { Session } from "@/bindings";
import type { PaletteCommand } from "@/store/palette";
import { resolveLabel } from "@/lib/sessions";

// ── Registry ──────────────────────────────────────────────────────────────────

export function buildCommands(args: {
  sessions: Session[];
  activeSessionId: string | null;
  sessionTitles: Record<string, string>;
}): PaletteCommand[] {
  const { sessions, activeSessionId, sessionTitles } = args;

  const actions: PaletteCommand[] = [
    {
      kind: "new-session",
      id: "action:new-session",
      title: "New session",
      keywords: "create start chat conversation",
      hint: "⌘N",
    },
    {
      kind: "toggle-split",
      id: "action:toggle-split",
      title: "Toggle split panel",
      keywords: "side panel open close view code output",
    },
    {
      kind: "toggle-wrap",
      id: "action:toggle-wrap",
      title: "Toggle word wrap",
      keywords: "split panel lines soft wrap",
    },
    {
      kind: "focus-composer",
      id: "action:focus-composer",
      title: "Focus composer",
      keywords: "message input write type reply prompt",
    },
  ];

  // Switching to the active session is a no-op, so it's left out. The ⌘1–9 hint
  // tracks each session's position in the full list (same mapping the sidebar and
  // useGlobalShortcuts use), so it stays accurate after the active one is removed.
  // resolveLabel is shared with the sidebar (lib/sessions.ts) so the switch list
  // and the sidebar filter can never diverge on a session's name.
  const sessionCmds: PaletteCommand[] = sessions
    .map((session, i) => ({ session, i }))
    .filter(({ session }) => session.id !== activeSessionId)
    .map(({ session, i }) => ({
      kind: "switch-session" as const,
      id: `session:${session.id}`,
      sessionId: session.id,
      title: resolveLabel(session, sessionTitles[session.id]),
      keywords: "session switch jump go to open",
      hint: i < 9 ? `⌘${i + 1}` : undefined,
    }));

  return [...actions, ...sessionCmds];
}

// ── Fuzzy filter ──────────────────────────────────────────────────────────────

// Subsequence match with bonuses for contiguous runs and word-boundary starts.
// Returns null when `query` isn't a subsequence of `target`; higher score is a
// better match. Small + synchronous — the registry is a handful of entries.
export function fuzzyScore(query: string, target: string): number | null {
  const q = query.toLowerCase();
  const t = target.toLowerCase();
  let qi = 0;
  let score = 0;
  let prev = -2;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] !== q[qi]) continue;
    let bonus = 1;
    if (ti === prev + 1) bonus += 3; // contiguous with the previous match
    if (ti === 0 || /[\s\-_/]/.test(t[ti - 1])) bonus += 2; // start of a word
    score += bonus;
    prev = ti;
    qi++;
  }
  return qi === q.length ? score : null;
}

export function filterCommands(
  commands: PaletteCommand[],
  query: string,
  recent: string[],
): PaletteCommand[] {
  const trimmed = query.trim();

  if (!trimmed) {
    // Empty query: most-recently-run first, otherwise registry order (stable sort).
    const rank = new Map(recent.map((id, i) => [id, i]));
    return [...commands].sort((a, b) => {
      const ra = rank.get(a.id) ?? Infinity;
      const rb = rank.get(b.id) ?? Infinity;
      return ra === rb ? 0 : ra - rb;
    });
  }

  return commands
    .map((cmd) => ({
      cmd,
      score: fuzzyScore(trimmed, `${cmd.title} ${cmd.keywords ?? ""}`),
    }))
    .filter(
      (x): x is { cmd: PaletteCommand; score: number } => x.score !== null,
    )
    .sort((a, b) => b.score - a.score)
    .map((x) => x.cmd);
}
