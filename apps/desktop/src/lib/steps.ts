// Pure helpers for the per-turn StepGroup (Issue #17). Kept free of React so the
// fold math + decision logic are unit-testable in isolation (see steps.test.ts).

import type { ToolStep } from "@/store/chat";

/** Rolling peek cap while a turn is streaming (Issue #180). */
export const STEP_WINDOW = 3;

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

/** Earliest tool:call timestamp in a turn — seeds the live elapsed timer. */
export function turnStartMs(steps: ToolStep[]): number | null {
  let firstStart = Infinity;
  for (const step of steps) {
    if (step.startedAt != null)
      firstStart = Math.min(firstStart, step.startedAt);
  }
  return firstStart === Infinity ? null : firstStart;
}

/** Live elapsed ms from turn start to `now` (streaming header). Prefers the
 *  earliest of wall-clock send time and the first tool:call when both exist. */
export function liveElapsedMs(
  steps: ToolStep[],
  now: number,
  wallClockStartMs?: number | null,
): number | null {
  const stepStart = turnStartMs(steps);
  const start =
    wallClockStartMs != null && stepStart != null
      ? Math.min(wallClockStartMs, stepStart)
      : (wallClockStartMs ?? stepStart);
  if (start === null) return null;
  return Math.max(0, now - start);
}

/**
 * Plain-text glimpse of a turn's answer for the collapsed StepGroup preview (#414).
 * Strips common Markdown syntax so the muted 2-line preview reads as prose, not
 * source — still the raw answer content, never a model-generated summary. Leaves
 * `_` alone so identifiers like `snake_case` survive.
 */
export function answerPreview(raw: string): string {
  return raw
    .replace(/```[\s\S]*?```/g, " ") // fenced code blocks
    .replace(/`([^`]+)`/g, "$1") // inline code
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ") // images
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1") // links → text
    .replace(/^\s{0,3}(#{1,6}|>)\s+/gm, "") // headings / blockquotes
    .replace(/^\s{0,3}([-*+]|\d+\.)\s+/gm, "") // list markers
    .replace(/[*~]/g, "") // bold/italic/strikethrough markers
    .replace(/\s+/g, " ") // collapse whitespace/newlines
    .trim();
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
 * Which turn items (steps + folded thoughts/reasoning, #687) to render inside an open
 * StepGroup. While streaming (and not awaiting approval/answer), only the last
 * {@link STEP_WINDOW} items show unless the user expands the peek via "+N earlier
 * steps". Windowing on items — not just steps — keeps the streaming view to recent
 * activity even when short thoughts are interleaved between tool calls.
 */
export function selectItemWindow<T>(
  items: T[],
  opts: {
    streaming: boolean;
    awaiting: boolean;
    peekExpanded: boolean;
  },
): { visible: T[]; hiddenCount: number } {
  const { streaming, awaiting, peekExpanded } = opts;
  if (awaiting || !streaming || peekExpanded || items.length <= STEP_WINDOW) {
    return { visible: items, hiddenCount: 0 };
  }
  return {
    visible: items.slice(-STEP_WINDOW),
    hiddenCount: items.length - STEP_WINDOW,
  };
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
