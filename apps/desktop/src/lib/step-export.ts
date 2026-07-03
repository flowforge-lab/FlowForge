// Dev-only step-timeline serializer (#417, #593). Turns a turn's interleaved
// `TurnItem[]` (per-iteration reasoning + tool steps, in the on-screen `foldTurns`
// order — #574) + run meta into a JSON dump (programmatic) or a CSV (sort by
// `durationMs` to find the bottleneck in one click) for diagnosing slow runs. Pure +
// React-free so it's unit-testable in isolation (see step-export.test.ts); the
// per-step timing it reads already lives in the store. This is a diagnostic
// affordance, NOT persisted-timing infrastructure — see the issue.

import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";
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

/** A tool-call row in the timeline. */
export interface TimelineStep {
  kind: "step";
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

/** A per-iteration reasoning row (#574/#593), interleaved in the position it was
 *  emitted — immediately before the steps that iteration produced. */
export interface TimelineReasoning {
  kind: "reasoning";
  index: number;
  /** Chain-of-thought text, kept in full (the CSV cell collapses whitespace). */
  text: string;
  /** Character count, for a quick size scan without expanding the row. */
  chars: number;
}

export type TimelineRow = TimelineStep | TimelineReasoning;

export interface TimelineDump {
  session: string;
  model: string | null;
  timing: TimingKind;
  /** ISO-8601 capture time. */
  capturedAt: string;
  totals: {
    steps: number;
    reasoning: number;
    totalMs: number | null;
    resultBytes: number;
  };
  /** Tool steps + reasoning interleaved in the on-screen (`foldTurns`) order (#593). */
  rows: TimelineRow[];
}

function summarizeStep(step: ToolStep, index: number): TimelineStep {
  const startedAt = step.startedAt ?? null;
  const finishedAt = step.finishedAt ?? null;
  const durationMs =
    startedAt != null && finishedAt != null
      ? Math.max(0, finishedAt - startedAt)
      : null;
  return {
    kind: "step",
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

function summarizeReasoning(text: string, index: number): TimelineReasoning {
  return { kind: "reasoning", index, text, chars: text.length };
}

/** Build the structured dump from a turn's interleaved items + run meta. Intermediate
 *  prose is top-level narration, not part of the step timeline, so it's dropped (#593);
 *  only `reasoning` and `step` items are serialized, in their original order. */
export function buildTimeline(
  items: TurnItem[],
  meta: TimelineMeta,
): TimelineDump {
  const rows: TimelineRow[] = [];
  const steps: ToolStep[] = [];
  for (const item of items) {
    if (item.kind === "step") {
      rows.push(summarizeStep(item.step, rows.length));
      steps.push(item.step);
    } else if (item.kind === "reasoning") {
      rows.push(summarizeReasoning(item.text, rows.length));
    }
    // `prose` items are intentionally skipped (out of scope for the step timeline).
  }
  const stepRows = rows.filter((r): r is TimelineStep => r.kind === "step");
  return {
    session: meta.sessionId,
    model: meta.model,
    timing: meta.timing,
    capturedAt: new Date(meta.capturedAt).toISOString(),
    totals: {
      steps: stepRows.length,
      reasoning: rows.length - stepRows.length,
      totalMs: groupDurationMs(steps),
      resultBytes: stepRows.reduce((n, s) => n + s.resultBytes, 0),
    },
    rows,
  };
}

export function timelineToJson(dump: TimelineDump): string {
  return JSON.stringify(dump, null, 2);
}

/** CSV columns, in order. `durationMs` is numeric so a spreadsheet sorts it; `kind`
 *  distinguishes reasoning rows from step rows, and `reasoning` carries their text. */
const CSV_COLUMNS = [
  "index",
  "kind",
  "tool",
  "status",
  "durationMs",
  "startedAt",
  "finishedAt",
  "resultBytes",
  "argsSummary",
  "reasoning",
] as const;

/** The cell value for a row/column — fields that don't apply to a row's kind are
 *  blank (empty tool/status/timing on reasoning rows; empty `reasoning` on steps). */
function csvValue(
  row: TimelineRow,
  col: (typeof CSV_COLUMNS)[number],
): string | number | null {
  switch (col) {
    case "index":
      return row.index;
    case "kind":
      return row.kind;
    case "tool":
      return row.kind === "step" ? row.tool : "";
    case "status":
      return row.kind === "step" ? row.status : "";
    case "durationMs":
      return row.kind === "step" ? row.durationMs : null;
    case "startedAt":
      return row.kind === "step" ? row.startedAt : null;
    case "finishedAt":
      return row.kind === "step" ? row.finishedAt : null;
    case "resultBytes":
      return row.kind === "step" ? row.resultBytes : "";
    case "argsSummary":
      return row.kind === "step" ? row.argsSummary : "";
    case "reasoning":
      // Collapse to a single line so the whole thought sits in one cell.
      return row.kind === "reasoning"
        ? row.text.replace(/\s+/g, " ").trim()
        : "";
  }
}

/** Quote a cell only when it contains a comma, quote, or newline (RFC 4180). */
function csvCell(value: string | number | null): string {
  const s = value == null ? "" : String(value);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function timelineToCsv(dump: TimelineDump): string {
  const header = CSV_COLUMNS.join(",");
  const rows = dump.rows.map((row) =>
    CSV_COLUMNS.map((col) => csvCell(csvValue(row, col))).join(","),
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
