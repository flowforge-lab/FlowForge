import { ipc } from "./ipc";

// A local model server (candle-vllm, Ollama, …) loses peak throughput while idle
// in two ways, on two timescales:
//
//   • Short (seconds): the GPU clocks down between turns, so the first token
//     after a brief pause crawls while the device ramps back up.
//   • Long (hours, e.g. overnight): the OS pages the model's weights out of RAM
//     under memory pressure, so the next generation faults them back in one slow
//     request before throughput recovers.
//
// A throwaway `ipc.warmup` turn covers both — it ramps the clock and faults the
// weights resident — so the user's first real message after returning streams at
// full speed instead of paying the ramp. We fire it on composer activity (focus /
// typing) and, crucially for the long-idle case, when the window regains focus
// (the user alt-tabbing back to the app after being away).
//
// Measured on Apple Silicon: short-idle warmth decays ~7-10s after activity, so
// the throttle sits just under that window. When already warm a nudge is ~0.4s of
// GPU; cold, it absorbs the ramp the real turn would otherwise pay.
export const WARMUP_THROTTLE_MS = 5_000;

/**
 * Rate-limits fire-and-forget server warmups so multiple triggers (composer
 * focus, typing, window refocus) collapse to at most one nudge per window. The
 * clock is injected so the throttle stays deterministic under test, mirroring
 * `TokenBatcher`.
 */
export class WarmupTrigger {
  private last = Number.NEGATIVE_INFINITY;

  constructor(
    private readonly warmup: () => void,
    private readonly throttleMs: number,
    private readonly now: () => number = () => Date.now(),
  ) {}

  /** Nudge the server unless one fired within the throttle window. */
  fire(): void {
    const t = this.now();
    if (t - this.last < this.throttleMs) return;
    this.last = t;
    this.warmup();
  }
}

/** Shared across every warmup trigger so they collapse onto one throttle. */
export const warmupTrigger = new WarmupTrigger(
  () => void ipc.warmup().catch(() => {}),
  WARMUP_THROTTLE_MS,
);

let focusWired = false;

/**
 * Warm the server when the window regains focus or becomes visible — the moment
 * the user returns after a long idle, before they've touched the composer. Glue
 * only (the throttle/decision lives in `WarmupTrigger`); idempotent and guarded
 * for non-DOM environments. Listeners live for the app lifetime.
 */
export function startWarmupOnFocus(): void {
  if (focusWired || typeof window === "undefined") return;
  focusWired = true;

  const fire = () => warmupTrigger.fire();
  window.addEventListener("focus", fire);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") fire();
  });
}
