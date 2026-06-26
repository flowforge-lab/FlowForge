/**
 * Classifies whether an assistant message's content is a *reason-bearing* stop
 * notice — the marker the agent loop writes when a turn ends without a usable
 * answer (the tool-call cap, a no-progress stall, or an empty model response).
 * See `crates/ff-agent/src/lib.rs` (`[stopped: …]` finalizer).
 *
 * Used to decide whether to offer the one-click "Continue" affordance (#513).
 * A *bare* `[stopped]` (a deliberate user cancel via the Stop button) is
 * deliberately excluded — re-running a turn the user chose to stop isn't the
 * target flow.
 *
 * This is the single place coupled to the notice's text shape. The robust
 * trigger upstream is structural (a turn that ends with empty streamed content);
 * this only classifies the refetched notice. A structured `stopReason` on
 * `TurnDoneEvent` would let us drop the string match entirely — tracked as a
 * #512 backend follow-up.
 */
export function isCappedNotice(content: string): boolean {
  return /^\[stopped:/.test(content.trim());
}
