// Composer slash-command registry + matcher (#1036). Unifies the three classes of
// `/command` the app has grown into one ranked list the composer can autocomplete:
//
//   builtin   `/goal`      — parsed by lib/goal-command.ts inside submit()
//   skill     `/grill-me`  — activates the skill (same call the ⌘K palette makes)
//   shortcut  `/ship`      — expands a stored message into the composer
//
// Pure and store-free, like lib/palette.ts and lib/goal-command.ts, so the gating
// and ranking rules are unit-testable under vitest's node env (the stores touch
// localStorage at module load and can't be imported there).

import type { SkillInfo } from "@/bindings";
import type { CommandShortcut } from "@/store/command-shortcuts";
import { fuzzyScore } from "@/lib/palette";

/** Which of the three worlds a row came from — drives the icon and the dispatch. */
export type SlashKind = "builtin" | "skill" | "shortcut";

/** One dropdown row. FE-shaped presentation, not a wire contract. */
export interface SlashCommand {
  /** Stable key, unique across the three sources. */
  id: string;
  kind: SlashKind;
  /** The invocation token WITHOUT the leading slash (e.g. `goal`, `grill-me`). */
  name: string;
  /** One-line explanation shown next to the token. */
  description: string;
  /** Right-aligned badge: "Active", "Won't apply", … */
  hint?: string;
  /** shortcut only — the message text to expand into the composer. */
  payload?: string;
  /** skill only — already in the active set, so accepting is a no-op. */
  active?: boolean;
}

/**
 * Builtin commands the composer handles itself. Only `/goal` today (#817); the
 * table is the extension point for the next one. Kept separate from the skill and
 * shortcut sources because a builtin's behavior lives in code, not in data.
 */
export const BUILTIN_SLASH_COMMANDS: readonly SlashCommand[] = [
  {
    id: "builtin:goal",
    kind: "builtin",
    name: "goal",
    description: "Start an autonomous goal for this session",
  },
];

/**
 * The dropdown's open/closed gate: the partial token being typed, or `null` when
 * this text isn't a slash command in progress.
 *
 * Open only while the caret is still inside a **leading** `/token` with no
 * whitespace yet — `/` → `""`, `/gr` → `"gr"`. Anything else is `null`:
 * a space ends the command name (`/goal ship it` is a `/goal` invocation being
 * typed, not a name to complete), a mid-line slash is ordinary prose (`see a/b`),
 * and leading whitespace is tolerated the way `parseGoalCommand` tolerates it.
 *
 * This whitespace rule is what keeps `/goal <objective>` working untouched: the
 * list closes the moment the user types the space before their objective, so
 * Enter goes back to submit() and the existing parse path.
 */
export function parseSlashQuery(raw: string): string | null {
  const text = raw.trimStart();
  if (!text.startsWith("/")) return null;
  const rest = text.slice(1);
  if (/\s/.test(rest)) return null;
  return rest;
}

/**
 * Merge the three sources into one registry.
 *
 * `sessionPhenotype` is the phenotype bound to the composer's session, if any.
 * It matters because the backend's `turn_active_skills` resolves a phenotype-bound
 * session from the phenotype's skill list and **ignores the global active set** —
 * so activating a skill would not reach that session's prompt. Rather than offer a
 * silent no-op, those rows are marked so the UI can say so.
 */
export function buildSlashCommands(args: {
  skills: SkillInfo[];
  shortcuts: CommandShortcut[];
  /** Name of the phenotype bound to this session, or null when unbound. */
  sessionPhenotype?: string | null;
}): SlashCommand[] {
  const { skills, shortcuts, sessionPhenotype = null } = args;

  const skillCmds: SlashCommand[] = skills.map((skill) => ({
    id: `skill:${skill.name}`,
    kind: "skill",
    name: skill.name,
    description: skill.description,
    active: skill.active,
    hint: sessionPhenotype
      ? "Won't apply"
      : skill.active
        ? "Active"
        : "Activate",
  }));

  const shortcutCmds: SlashCommand[] = shortcuts.map((s) => ({
    id: `shortcut:${s.id}`,
    kind: "shortcut",
    name: s.name,
    description: s.message,
    payload: s.message,
    hint: "Expand",
  }));

  return [...BUILTIN_SLASH_COMMANDS, ...skillCmds, ...shortcutCmds];
}

/**
 * Rank the registry against the partial token from `parseSlashQuery`.
 *
 * Empty query (bare `/`) lists everything in registry order — builtins first, so
 * the discoverable commands lead. A typed query reuses the palette's `fuzzyScore`
 * (subsequence + contiguity/word-boundary bonuses) so ranking behaves exactly like
 * ⌘K. The name is scored on its own first: a token match must outrank a row that
 * only matches deep in its description.
 */
export function matchSlash(
  commands: SlashCommand[],
  query: string,
): SlashCommand[] {
  const q = query.trim();
  if (q === "") return [...commands];

  return commands
    .map((cmd) => {
      const nameScore = fuzzyScore(q, cmd.name);
      const textScore = fuzzyScore(q, `${cmd.name} ${cmd.description}`);
      if (nameScore === null && textScore === null) return null;
      // Weight a name hit above a description-only hit so `/gr` puts `grill-me`
      // over a skill that merely mentions "grill" in its blurb.
      const score = (nameScore ?? 0) * 10 + (textScore ?? 0);
      return { cmd, score };
    })
    .filter((x): x is { cmd: SlashCommand; score: number } => x !== null)
    .sort((a, b) => b.score - a.score)
    .map((x) => x.cmd);
}
