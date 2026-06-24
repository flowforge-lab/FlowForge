// Dev-only step-timeline serializer (#417). Turns a turn's `ToolStep[]` + run meta
// into a JSON dump (programmatic) or a CSV (sort by `durationMs` to find the
// bottleneck in one click) for diagnosing slow runs. Pure + React-free so it's
// unit-testable in isolation (see step-export.test.ts); the per-step timing it reads
// already lives in the store. This is a diagnostic affordance, NOT persisted-timing
// infrastructure — see the issue.

import type { ToolStep } from "@/store/chat";
import { formatArgs } from "@/lib/tool-args";
import { groupDurationMs } from "@/lib/steps";

/** Whether the step timing is real (`exact`, live `startedAt`/`finishedAt`) or coarse
 *  (`approx-created-at`, derived from message `createdAt` on a reloaded turn) — so a
 *  coarse dump is never mistaken for exact when analyzing. */
export type TimingKind = "exact" | "approx-created-at";

export interface TimelineMeta {
  sessionId: string;
  /** Active model id, or null when unknown. */
  model: string | null;
  timing: TimingKind;
  /** Epoch ms when the dump was captured (e.g. `Date.now()`). */
  capturedAt: number;
}

export interface TimelineStep {
  index: number;
  tool: string;
  /** Compact single-line args summary (truncated by {@link formatArgs}). */
  argsSummary: string;
  status: ToolStep["status"];
  startedAt: number | null;
  finishedAt: number | null;
  durationMs: number | null;
  resultBytes: number;
}

export interface TimelineDump {
  session: string;
  model: string | null;
  timing: TimingKind;
  /** ISO-8601 capture time. */
  capturedAt: string;
  totals: { steps: number; totalMs: number | null; resultBytes: number };
  steps: TimelineStep[];
}

function summarizeStep(step: ToolStep, index: number): TimelineStep {
  const startedAt = step.startedAt ?? null;
  const finishedAt = step.finishedAt ?? null;
  const durationMs =
    startedAt != null && finishedAt != null
      ? Math.max(0, finishedAt - startedAt)
      : null;
  return {
    index,
    tool: step.tool,
    // Reuse the existing arg formatter (which truncates long strings), collapsed to
    // a single line so it sits in one CSV cell.
    argsSummary: formatArgs(step.args).replace(/\s+/g, " ").trim(),
    status: step.status,
    startedAt,
    finishedAt,
    durationMs,
    resultBytes: step.result?.length ?? 0,
  };
}

/** Build the structured dump from a turn's steps + run meta. */
export function buildTimeline(
  steps: ToolStep[],
  meta: TimelineMeta,
): TimelineDump {
  const timelineSteps = steps.map(summarizeStep);
  return {
    session: meta.sessionId,
    model: meta.model,
    timing: meta.timing,
    capturedAt: new Date(meta.capturedAt).toISOString(),
    totals: {
      steps: timelineSteps.length,
      totalMs: groupDurationMs(steps),
      resultBytes: timelineSteps.reduce((n, s) => n + s.resultBytes, 0),
    },
    steps: timelineSteps,
  };
}

export function timelineToJson(dump: TimelineDump): string {
  return JSON.stringify(dump, null, 2);
}

/** CSV columns, in order. `durationMs` is numeric so a spreadsheet sorts it. */
const CSV_COLUMNS = [
  "index",
  "tool",
  "status",
  "durationMs",
  "startedAt",
  "finishedAt",
  "resultBytes",
  "argsSummary",
] as const satisfies readonly (keyof TimelineStep)[];

/** Quote a cell only when it contains a comma, quote, or newline (RFC 4180). */
function csvCell(value: string | number | null): string {
  const s = value == null ? "" : String(value);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function timelineToCsv(dump: TimelineDump): string {
  const header = CSV_COLUMNS.join(",");
  const rows = dump.steps.map((step) =>
    CSV_COLUMNS.map((col) => csvCell(step[col])).join(","),
  );
  return [header, ...rows].join("\n") + "\n";
}

/** A safe download filename, e.g. `step-timeline-06d8ba6e.csv`. */
export function timelineFilename(
  dump: TimelineDump,
  ext: "json" | "csv",
): string {
  const stem = dump.session.slice(0, 8) || "session";
  return `step-timeline-${stem}.${ext}`;
}
