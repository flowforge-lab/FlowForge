import { describe, it, expect } from "vitest";
import {
  parseKernelStatus,
  parseNotebookStep,
  isNotebookRunnerStep,
  type NotebookAction,
} from "@/lib/notebook-output";

describe("isNotebookRunnerStep", () => {
  it("matches the canonical tool name", () => {
    expect(isNotebookRunnerStep("notebook_runner")).toBe(true);
  });

  it("rejects unrelated tool names", () => {
    expect(isNotebookRunnerStep("bash")).toBe(false);
    expect(isNotebookRunnerStep("notebook")).toBe(false);
    expect(isNotebookRunnerStep("")).toBe(false);
  });
});

describe("parseNotebookStep", () => {
  const cases: NotebookAction[] = ["start", "run_cell", "status", "stop"];

  it.each(cases)(
    "returns null for non-notebook tool names (action=%s)",
    (action) => {
      expect(parseNotebookStep("bash", { action }, "ok", "done")).toBeNull();
    },
  );

  it("normalizes a successful run_cell with stdout", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "print(1+1)" },
      "2\n",
      "done",
    );
    expect(parsed).toMatchObject({
      action: "run_cell",
      code: "print(1+1)",
      // Trailing newline trimmed — the renderer shouldn't need to.
      output: "2",
      errored: false,
      parsedExceptionTrailer: false,
      isStatusReport: false,
    });
    // The exact normalization is asserted here (covered by the trailer test
    // for the no-trailer branch) — we just need to confirm the success case
    // also drops trailing whitespace so the cell view doesn't render a blank
    // line.
    expect(parsed!.output).toBe("2");
  });

  it("flags errored and strips the canonical exception trailer", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "raise ValueError('boom')" },
      "Traceback (most recent call last):\n  ...\nValueError: boom\n[cell raised an exception]\n",
      "done",
    );
    expect(parsed).not.toBeNull();
    expect(parsed!.errored).toBe(true);
    expect(parsed!.parsedExceptionTrailer).toBe(true);
    // Trailer stripped, trailing newlines trimmed, no leftover "[cell raised".
    expect(parsed!.output).toContain("ValueError: boom");
    expect(parsed!.output).not.toContain("[cell raised an exception]");
  });

  it("flags errored when the step status is error even without a trailer", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "1/0" },
      "ZeroDivisionError: division by zero",
      "error",
    );
    expect(parsed?.errored).toBe(true);
    expect(parsed?.parsedExceptionTrailer).toBe(false);
  });

  it("renders status action with the canonical summary line", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "status" },
      "kernel kernel-1a2b3c4d — running; pid=4242; cells executed=3",
      "done",
    );
    expect(parsed?.action).toBe("status");
    expect(parsed?.isStatusReport).toBe(true);
    expect(parsed?.output).toContain("kernel-1a2b3c4d");
  });

  it("collapses unknown / missing actions into a status report", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "WHAT" },
      "anything",
      "done",
    );
    expect(parsed?.action).toBe("status");
    expect(parsed?.isStatusReport).toBe(true);
  });

  it("renders start / stop as plain result text", () => {
    const start = parseNotebookStep(
      "notebook_runner",
      { action: "start" },
      "started kernel kernel-aaaaaaaa; run code with action=run_cell",
      "done",
    );
    expect(start).toMatchObject({
      action: "start",
      code: null,
      errored: false,
      isStatusReport: false,
    });
    expect(start?.output).toContain("started kernel");

    const stop = parseNotebookStep(
      "notebook_runner",
      { action: "stop" },
      "stopped kernel kernel-aaaaaaaa",
      "done",
    );
    expect(stop?.action).toBe("stop");
  });

  it("treats a running step with no result as a live cell", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "x = 1" },
      undefined,
      "running",
    );
    expect(parsed?.output).toBe("");
    expect(parsed?.errored).toBe(false);
  });
});

describe("parseKernelStatus", () => {
  it("parses a live kernel line", () => {
    const s = parseKernelStatus(
      "kernel kernel-1a2b3c4d — running; pid=1234; cells executed=7",
    );
    expect(s).toEqual({
      state: "live",
      kernelId: "kernel-1a2b3c4d",
      executionCount: 7,
      raw: "kernel kernel-1a2b3c4d — running; pid=1234; cells executed=7",
    });
  });

  it("parses a dead kernel line", () => {
    const s = parseKernelStatus(
      "kernel kernel-deadbeef — dead; pid=42; cells executed=0",
    );
    expect(s.state).toBe("dead");
    expect(s.kernelId).toBe("kernel-deadbeef");
    expect(s.executionCount).toBe(0);
  });

  it("detects the no-kernel prefix case-insensitively", () => {
    expect(parseKernelStatus("no kernel running for this session").state).toBe(
      "no-kernel",
    );
    expect(parseKernelStatus("No kernel running for this session").state).toBe(
      "no-kernel",
    );
  });

  it("falls back to unknown for unparseable input", () => {
    const s = parseKernelStatus("hello world");
    expect(s.state).toBe("unknown");
    expect(s.kernelId).toBeNull();
    expect(s.executionCount).toBeNull();
  });
});
