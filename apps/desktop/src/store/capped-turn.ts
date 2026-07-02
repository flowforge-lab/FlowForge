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
 * As of #658 the primary classifier is the structured `stopReason` on
 * `TurnDoneEvent` / `Message`. This string match is retained as the documented
 * **legacy fallback**: rows persisted before #658 carry only `content`, so
 * deleting it would silently mis-render historical stopped turns. New turns take
 * the structured path; this only fires when `stopReason` is absent.
 */
export function isResumableStopNotice(content: string): boolean {
  return /^\[stopped/.test(content.trim());
}
