import type { TokenEvent } from "@/bindings";

/**
 * Coalesces high-frequency token events so the chat store commits at most once per
 * scheduled tick instead of once per token. At 40-50 tok/s a per-token commit
 * re-renders (and re-parses markdown for) the streaming message tens of times a
 * second; batching to animation-frame cadence cuts that to ~60/s while preserving
 * order and content (#104).
 *
 * Deltas are accumulated per message, so a flush applies one concatenated delta per
 * message. The scheduler is injected (production passes `requestAnimationFrame`) so
 * the batching logic stays deterministic under test.
 */
export class TokenBatcher {
  private readonly pending = new Map<string, TokenEvent>();
  private scheduled = false;

  constructor(
    private readonly flush: (e: TokenEvent) => void,
    private readonly schedule: (cb: () => void) => void,
  ) {}

  /** Queue a token; schedules a drain on the first token of each tick. */
  push(e: TokenEvent): void {
    const prev = this.pending.get(e.messageId);
    this.pending.set(
      e.messageId,
      prev ? { ...e, delta: prev.delta + e.delta } : { ...e },
    );
    if (!this.scheduled) {
      this.scheduled = true;
      this.schedule(() => this.drain());
    }
  }

  /**
   * Flush all queued deltas immediately. Call before any non-token event (turn
   * done/error, tool call) so buffered text lands before that event is handled —
   * otherwise a finish could clear the streaming flag ahead of the last tokens.
   */
  drain(): void {
    this.scheduled = false;
    if (this.pending.size === 0) return;
    const items = [...this.pending.values()];
    this.pending.clear();
    for (const e of items) this.flush(e);
  }
}
