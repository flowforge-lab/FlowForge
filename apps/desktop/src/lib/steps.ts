// Pure helpers for the per-turn StepGroup (Issue #17). Kept free of React so the
// fold math + decision logic are unit-testable in isolation (see steps.test.ts).

import type { ToolStep } from "@/store/chat";

/**
 * Wall-clock duration of a turn's tool steps: last finish − first start.
 * Returns null until at least one step has both started and a result has landed
 * (i.e. mid-stream, before the first `tool:result`), so callers can hold off on
 * rendering a duration that isn't meaningful yet.
 */
export function groupDurationMs(steps: ToolStep[]): number | null {
  let firstStart = Infinity;
  let lastFinish = -Infinity;
  for (const step of steps) {
    if (step.startedAt != null)
      firstStart = Math.min(firstStart, step.startedAt);
    if (step.finishedAt != null)
      lastFinish = Math.max(lastFinish, step.finishedAt);
  }
  if (firstStart === Infinity || lastFinish === -Infinity) return null;
  return Math.max(0, lastFinish - firstStart);
}

/** Compact, human duration matching the reference transcript: "<1s", "8s", "1m 3s". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return "<1s";
  const totalSec = Math.round(ms / 1000);
  if (totalSec < 60) return `${totalSec}s`;
  const min = Math.floor(totalSec / 60);
  const sec = totalSec % 60;
  return sec ? `${min}m ${sec}s` : `${min}m`;
}

/**
 * Whether a StepGroup is expanded.
 * - Approval forces it open — the Approve/Deny buttons must never be hidden.
 * - Otherwise a manual user toggle wins and persists through `turn:done`.
 * - Otherwise it follows the turn: expanded while streaming, collapsed once settled.
 */
export function resolveGroupOpen(args: {
  awaiting: boolean;
  /** null = untouched (follow the turn); true/false = an explicit user choice. */
  userOpen: boolean | null;
  streaming: boolean;
}): boolean {
  if (args.awaiting) return true;
  if (args.userOpen !== null) return args.userOpen;
  return args.streaming;
}
