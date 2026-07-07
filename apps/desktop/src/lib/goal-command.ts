// Parse the `/goal` composer slash command (#817, RFC 0020 §6). The composer's
// primary entry to Goal mode besides the command palette's "Start goal…" dialog
// (#816): typing `/goal <objective>` starts a goal for the active session instead
// of sending a chat message.
//
// Kept as a pure function (no React / store deps) so the parse rules are unit-
// testable in isolation and the composer's `submit()` stays thin.

/** The outcome of interpreting a composer submission against the `/goal` grammar. */
export type GoalCommand =
  | { kind: "not-a-command" }
  | { kind: "start"; objective: string }
  | { kind: "open-dialog" };

/**
 * Classify a raw composer string.
 *
 * - `/goal <objective>` → `start` with the trimmed objective.
 * - `/goal` (or `/goal` + only whitespace) → `open-dialog` — defer to the
 *   start-goal dialog (#816) rather than erroring, so a bare command is a
 *   discoverable affordance.
 * - anything else (including text that merely contains `/goal` mid-line, or a
 *   different slash token) → `not-a-command`; the composer sends it normally.
 *
 * Only a leading `/goal` token (case-insensitive) delimited by end-of-string or
 * whitespace matches — `/goalpost ...` is NOT a command, and `foo /goal` is a
 * normal message.
 */
export function parseGoalCommand(raw: string): GoalCommand {
  const text = raw.trimStart();
  // Match `/goal` at the start, followed by end-of-string or whitespace. The
  // `\s` guard prevents `/goalpost` from being treated as `/goal`.
  const match = /^\/goal(\s+([\s\S]*))?$/i.exec(text);
  if (!match) return { kind: "not-a-command" };
  const objective = (match[2] ?? "").trim();
  return objective.length > 0
    ? { kind: "start", objective }
    : { kind: "open-dialog" };
}
