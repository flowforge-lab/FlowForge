// Render model for the `notebook_runner` tool (#859 / #856 / #871, epic #856).
//
// The backend is currently text-only (Phase 1 + Phase 2 of the notebook_runner
// epic — #863, #870): `notebook_runner` returns a plain `ToolResultEvent` whose
// `result` carries the cell stdout/stderr (or, for `action=status`, a one-line
// kernel-state summary). The transcript renderer needs a notebook-styled cell
// view NOW so the affordance exists in #871 (FE-2) without waiting for Phase 3
// (image / variable payload, structured CellOutput).
//
// This file is the single source of truth for normalizing that text into a
// shape the renderer can lay out: action kind, the `code` arg (for `run_cell`),
// the textual output, and an ok/error status. The detector is exported as
// `isNotebookRunnerStep` so `tool-step.tsx` can branch once.
//
// Future Phase 3 (rich output — image path + variable dump) will extend
// `NotebookOutput` with `images?: { mediaType: string; dataUrl: string }[]` and
// `variables?: Array<{ name: string; type?: string; repr: string }>`. The
// renderer (`notebook-cell-output.tsx`) already gates those blocks on presence,
// so adding them later is an additive change to this parser + the renderer,
// with no re-plumbing in `tool-step.tsx`. Per the issue's FE-0 contract note:
// the FE does not invent the shape — it lands with the Phase 3 backend.
//
// Kept React-free so the parse path is unit-testable in vitest's node env
// (mirrors `lib/todo.ts`).

/** The four `notebook_runner` actions the tool currently understands. */
export type NotebookAction = "start" | "run_cell" | "status" | "stop";

/** What the FE renders for a step. `null` = "this isn't a notebook call". */
export interface NotebookStep {
  /** The action the agent invoked (drive different layouts). */
  action: NotebookAction;
  /** For `run_cell`, the Python source the model supplied. */
  code: string | null;
  /** Final textual result (cell output, status line, or start/stop notice). */
  output: string;
  /** `true` when the cell raised an exception; `false` for any other action. */
  errored: boolean;
  /**
   * `true` when the parser saw the canonical exception trailer the backend
   * appends (`\n[cell raised an exception]`, set by `notebook/mod.rs:run_cell`).
   * The trailer is stripped from `output` for the cell view, but the flag
   * survives so the renderer can show the red ok/error badge.
   */
  parsedExceptionTrailer: boolean;
  /** True when the action was `status` (drives the kernel-state pill). */
  isStatusReport: boolean;
}

/** Matches a single notebook_runner step, regardless of the source field shape. */
export function isNotebookRunnerStep(tool: string): boolean {
  return tool === "notebook_runner";
}

/** Best-effort extract of the `action` string from a step's `args` payload. */
function readAction(args: unknown): NotebookAction | null {
  if (!args || typeof args !== "object") return null;
  const raw = (args as { action?: unknown }).action;
  if (typeof raw !== "string") return null;
  switch (raw) {
    case "start":
    case "run_cell":
    case "status":
    case "stop":
      return raw;
    default:
      return null;
  }
}

/** Read the `code` arg if it's a string (anything else collapses to null). */
function readCode(args: unknown): string | null {
  if (!args || typeof args !== "object") return null;
  const raw = (args as { code?: unknown }).code;
  return typeof raw === "string" ? raw : null;
}

// The backend appends this literal trailer to a `run_cell` result when the
// driver reported `error` (see `notebook_runner run_cell` arm in
// `crates/ff-tools/src/notebook/mod.rs`). Kept as a constant so a future
// backend change to the trailer text only touches one place.
const EXCEPTION_TRAILER = "[cell raised an exception]";

/**
 * Normalize a notebook_runner step's args + result into a render model.
 * Returns `null` for anything that doesn't look like a notebook call so the
 * caller can fall back to the generic step render path.
 */
export function parseNotebookStep(
  tool: string,
  args: unknown,
  result: string | undefined,
  status: "running" | "done" | "error",
): NotebookStep | null {
  if (!isNotebookRunnerStep(tool)) return null;
  const action = readAction(args) ?? "status";
  // While the call is in flight we may not have a result yet; the step view
  // falls back to the live `output` stream. While idle, prefer the canonical
  // `result` (the tool-result event's body).
  const raw = result ?? "";

  if (action === "status") {
    return {
      action,
      code: null,
      output: raw,
      // The `status` action is read-only; the only way it errors is if the
      // backend reports no kernel (the result text starts with "no kernel …").
      errored: status === "error",
      parsedExceptionTrailer: false,
      isStatusReport: true,
    };
  }

  if (action === "run_cell") {
    const code = readCode(args);
    // Strip the trailer before the cell render — we render the red ok/error
    // badge separately so the user sees the exception banner above a clean
    // output block, not an extra line at the end. Also drop any trailing
    // whitespace so the cell view doesn't render a blank final line.
    const trailerIdx = raw.lastIndexOf(EXCEPTION_TRAILER);
    const parsedExceptionTrailer = trailerIdx >= 0;
    const output = (
      parsedExceptionTrailer ? raw.slice(0, trailerIdx) : raw
    ).replace(/\n+$/, "");
    // The backend reports success/failure in the ToolResultEvent; we cross-check
    // with the trailer so a stale "ok" status + an exception body still flags
    // the cell as errored.
    const errored = status === "error" || parsedExceptionTrailer;
    return {
      action,
      code,
      output,
      errored,
      parsedExceptionTrailer,
      isStatusReport: false,
    };
  }

  // `start` / `stop` — render the result text neutrally.
  return {
    action,
    code: null,
    output: raw,
    errored: status === "error",
    parsedExceptionTrailer: false,
    isStatusReport: false,
  };
}

/**
 * Parse the canonical `notebook_runner status` line into a structured summary
 * suitable for a status pill. The backend emits, per session:
 *   `kernel <id> — <running|dead>; pid=<pid>; cells executed=<n>`
 * …or, when no kernel exists, `no kernel running for this session`. Both are
 * parsed; anything else falls back to the raw line so the UI never shows a
 * blank pill.
 */
export interface KernelStatus {
  /** "no kernel" when the session has no kernel, "live" / "dead" otherwise. */
  state: "no-kernel" | "live" | "dead" | "unknown";
  /** The kernel id (e.g. `kernel-abc12345`), if the line carried one. */
  kernelId: string | null;
  /** The cell execution count (`cells executed=N`), if the line carried one. */
  executionCount: number | null;
  /** Original text (for the fallback pill). */
  raw: string;
}

const NO_KERNEL_PREFIX = "no kernel";

export function parseKernelStatus(text: string): KernelStatus {
  const trimmed = text.trim();
  if (trimmed.toLowerCase().startsWith(NO_KERNEL_PREFIX)) {
    return {
      state: "no-kernel",
      kernelId: null,
      executionCount: null,
      raw: text,
    };
  }
  // Examples: "kernel kernel-1a2b3c4d — running; pid=1234; cells executed=7"
  //           "kernel kernel-1a2b3c4d — dead; pid=1234; cells executed=7"
  const idMatch = /^kernel\s+(\S+)\s+—\s+(\w+)/.exec(trimmed);
  const countMatch = /cells\s+executed\s*=\s*(\d+)/.exec(trimmed);
  if (!idMatch) {
    return {
      state: "unknown",
      kernelId: null,
      executionCount: null,
      raw: text,
    };
  }
  const word = idMatch[2].toLowerCase();
  const state: KernelStatus["state"] =
    word === "running" ? "live" : word === "dead" ? "dead" : "unknown";
  return {
    state,
    kernelId: idMatch[1],
    executionCount: countMatch ? Number(countMatch[1]) : null,
    raw: text,
  };
}
