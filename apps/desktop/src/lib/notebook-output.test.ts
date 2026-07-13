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
  const cases: NotebookAction[] = [
    "start",
    "run_cell",
    "run_all",
    "status",
    "stop",
    "restart",
    "inspect",
  ];

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

  it("does NOT flag a successful cell whose stdout contains the trailer string in the middle", () => {
    // `lastIndexOf` would false-positive here; the new heuristic only strips
    // the trailer when it is the exact final non-whitespace line of the body
    // (the shape the backend actually emits — `\n[cell raised an exception]`).
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "x = 1" },
      "[cell raised an exception]\nthis happened earlier\n",
      "done",
    );
    expect(parsed?.parsedExceptionTrailer).toBe(false);
    expect(parsed?.errored).toBe(false);
    // Output untouched — the printed string stays where the model put it.
    expect(parsed?.output).toContain("[cell raised an exception]");
    expect(parsed?.output).toContain("this happened earlier");
  });

  it("does NOT flag a successful cell that prints the trailer as its final line", () => {
    // The reviewer's exact edge case: `print("[cell raised an exception]")`.
    // The printed line has no preceding newline, whereas the backend always
    // emits the real trailer as `\n[cell raised an exception]`, so requiring
    // the leading newline leaves this successful cell alone.
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: 'print("[cell raised an exception]")' },
      "[cell raised an exception]\n",
      "done",
    );
    expect(parsed?.parsedExceptionTrailer).toBe(false);
    expect(parsed?.errored).toBe(false);
    expect(parsed?.output).toBe("[cell raised an exception]");
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

  it("renders Phase-3 `restart` as plain result text (not a status report)", () => {
    // Pre-declared in #871 FE-0 so Phase 3 doesn't widen the union retroactively.
    // The current backend doesn't emit it yet, so the parser must accept it now
    // and render it the same way as `stop` (neutral, non-status).
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "restart" },
      "restarted kernel kernel-aaaaaaaa",
      "done",
    );
    expect(parsed?.action).toBe("restart");
    expect(parsed?.isStatusReport).toBe(false);
    expect(parsed?.errored).toBe(false);
    expect(parsed?.output).toBe("restarted kernel kernel-aaaaaaaa");
  });

  it("renders Phase-3 `run_all` / `inspect` with the same neutral layout as `stop`", () => {
    const runAll = parseNotebookStep(
      "notebook_runner",
      { action: "run_all" },
      "ran 3 cells\n",
      "done",
    );
    expect(runAll?.action).toBe("run_all");
    expect(runAll?.isStatusReport).toBe(false);
    expect(runAll?.output).toBe("ran 3 cells");

    const inspect = parseNotebookStep(
      "notebook_runner",
      { action: "inspect" },
      "x: int = 5",
      "done",
    );
    expect(inspect?.action).toBe("inspect");
    expect(inspect?.isStatusReport).toBe(false);
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

describe("parseNotebookStep — Phase 3 FF_NB_META trailer (#879)", () => {
  it("strips the trailer and populates images for a run_cell figure", () => {
    const trailer =
      '\n<<<FF_NB_META\n{"images":[{"path":"/tmp/flowforge-notebook/kernel-ab12/fig1.png","mediaType":"image/png"}],"variables":[]}\nFF_NB_META\n';
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "plt.plot([1,2,3])" },
      `plotted.${trailer}`,
      "done",
    );
    expect(parsed?.output).toBe("plotted.");
    expect(parsed?.errored).toBe(false);
    expect(parsed?.images).toEqual([
      {
        path: "/tmp/flowforge-notebook/kernel-ab12/fig1.png",
        mediaType: "image/png",
      },
    ]);
    // The trailer JSON always carries both keys (one may be empty) — the
    // parser attaches them together as one payload rather than omitting the
    // empty side, so this is `[]`, not `undefined`.
    expect(parsed?.variables).toEqual([]);
  });

  it("strips the trailer and populates variables for an inspect dump", () => {
    const trailer =
      '\n<<<FF_NB_META\n{"images":[],"variables":[{"name":"df","type":"DataFrame","repr":"<5 rows>"}]}\nFF_NB_META\n';
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "inspect" },
      `1 variable(s) in scope:\n  df: DataFrame = <5 rows>${trailer}`,
      "done",
    );
    expect(parsed?.action).toBe("inspect");
    expect(parsed?.output).toBe(
      "1 variable(s) in scope:\n  df: DataFrame = <5 rows>",
    );
    expect(parsed?.variables).toEqual([
      { name: "df", type: "DataFrame", repr: "<5 rows>" },
    ]);
    // Same reasoning as above, mirrored: `images` is `[]`, not `undefined`.
    expect(parsed?.images).toEqual([]);
  });

  it("populates both images and variables when a single trailer carries both", () => {
    const trailer =
      '\n<<<FF_NB_META\n{"images":[{"path":"/tmp/fig.png","mediaType":"image/png"}],"variables":[{"name":"x","repr":"5"}]}\nFF_NB_META\n';
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "x = 5; plt.plot([x])" },
      `ok${trailer}`,
      "done",
    );
    expect(parsed?.images).toEqual([
      { path: "/tmp/fig.png", mediaType: "image/png" },
    ]);
    // `type` is optional on NotebookVariable — omitted here on purpose.
    expect(parsed?.variables).toEqual([{ name: "x", repr: "5" }]);
  });

  it("leaves images/variables undefined when there is no trailer (regression)", () => {
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "print(1)" },
      "1\n",
      "done",
    );
    expect(parsed?.output).toBe("1");
    expect(parsed?.images).toBeUndefined();
    expect(parsed?.variables).toBeUndefined();
  });

  it("degrades to plain text when the FF_NB_META payload is malformed JSON", () => {
    const badTrailer = "\n<<<FF_NB_META\n{not valid json\nFF_NB_META\n";
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "print(1)" },
      `1${badTrailer}`,
      "done",
    );
    expect(parsed?.images).toBeUndefined();
    expect(parsed?.variables).toBeUndefined();
    // Malformed trailer is left in place rather than silently eaten — the
    // user can still read what the backend sent.
    expect(parsed?.output).toContain("FF_NB_META");
  });

  it("strips the meta trailer before detecting the exception trailer (ordering)", () => {
    // The backend appends the meta trailer *after* the exception trailer, so
    // the parser must strip it first or the exception check never matches.
    const trailer =
      '\n<<<FF_NB_META\n{"images":[{"path":"/tmp/partial.png","mediaType":"image/png"}],"variables":[]}\nFF_NB_META\n';
    const parsed = parseNotebookStep(
      "notebook_runner",
      { action: "run_cell", code: "plt.plot([1]); 1/0" },
      `Traceback...\nZeroDivisionError: division by zero\n[cell raised an exception]${trailer}`,
      "done",
    );
    expect(parsed?.errored).toBe(true);
    expect(parsed?.parsedExceptionTrailer).toBe(true);
    expect(parsed?.output).toContain("ZeroDivisionError");
    expect(parsed?.output).not.toContain("[cell raised an exception]");
    expect(parsed?.output).not.toContain("FF_NB_META");
    expect(parsed?.images).toEqual([
      { path: "/tmp/partial.png", mediaType: "image/png" },
    ]);
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
