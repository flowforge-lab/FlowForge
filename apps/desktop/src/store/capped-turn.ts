/**
 * Classifies whether an assistant message's content is a *resumable* stop
 * notice — the marker the agent loop writes when a turn ends without a usable
 * answer that the user can pick back up with one click.
 *
 * Two shapes qualify:
 *   - `[stopped: …]` — a reason-bearing finalizer (the tool-call cap, a
 *     no-progress stall, or an empty model response). See `crates/ff-agent/src/lib.rs`.
 *   - `[stopped]`    — a bare marker written when the *user* cancels a turn via
 *     the Stop button (`cancel.is_cancelled()`). Product wants the same one-click
 *     resume here (#636), so it is now included — it used to be excluded.
 *
 * Used to decide whether to offer the one-click "Continue" affordance (#513/#636).
 *
 * This is the single place coupled to the notice's text shape. The robust
 * trigger upstream is structural (a turn that ends with empty streamed content);
 * this only classifies the refetched notice. A structured `stopReason` on
 * `TurnDoneEvent` would let us drop the string match entirely — tracked as a
 * #512 backend follow-up.
 */
export function isResumableStopNotice(content: string): boolean {
  return /^\[stopped/.test(content.trim());
}
