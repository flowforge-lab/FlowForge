// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotebookCellOutput } from "@/components/notebook-cell-output";
import type { ToolStep } from "@/store/chat";

const { readFile } = vi.hoisted(() => ({ readFile: vi.fn() }));
vi.mock("@tauri-apps/plugin-fs", () => ({ readFile }));

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function renderStep(step: ToolStep) {
  act(() => {
    root.render(<NotebookCellOutput step={step} />);
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  readFile.mockReset();
  act(() => {
    root = createRoot(container);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("NotebookCellOutput (#871 FE-2)", () => {
  it("renders a successful run_cell with code + output + ok badge", () => {
    renderStep({
      callId: "c1",
      tool: "notebook_runner",
      args: { action: "run_cell", code: "print(2 + 2)" },
      status: "done",
      result: "4",
    });
    const text = container.textContent ?? "";
    expect(text).toContain("print(2 + 2)");
    expect(text).toContain("4");
    expect(text.toLowerCase()).toContain("ok");
    // No error styling — output pre is the neutral foreground class.
    const pre = container.querySelector("pre");
    expect(pre?.className).toContain("text-foreground/90");
  });

  it("flags an errored cell and strips the canonical trailer", () => {
    renderStep({
      callId: "c2",
      tool: "notebook_runner",
      args: { action: "run_cell", code: "1/0" },
      status: "error",
      result:
        "ZeroDivisionError: division by zero\n[cell raised an exception]\n",
    });
    const text = container.textContent ?? "";
    expect(text).toContain("ZeroDivisionError");
    expect(text).not.toContain("[cell raised an exception]");
    // Destructive badge present.
    const badges = container.querySelectorAll('[data-slot="badge"]');
    expect(
      [...badges].some((b) => /exception/i.test(b.textContent ?? "")),
    ).toBe(true);
  });

  it("surfaces a `no kernel` status line as quiet text", () => {
    renderStep({
      callId: "c3",
      tool: "notebook_runner",
      args: { action: "status" },
      status: "done",
      result: "no kernel running for this session",
    });
    const text = container.textContent ?? "";
    expect(text).toContain("no kernel for this session");
    // No green pill (no kernel ≠ running).
    expect(container.querySelector(".bg-emerald-500")).toBeNull();
  });

  it("surfaces a live kernel pill with id + execution count", () => {
    renderStep({
      callId: "c4",
      tool: "notebook_runner",
      args: { action: "status" },
      status: "done",
      result: "kernel kernel-1a2b3c4d — running; pid=1234; cells executed=3",
    });
    const text = container.textContent ?? "";
    expect(text).toContain("kernel running");
    expect(text).toContain("kernel-1a2b3c4d");
    expect(text).toContain("3 cells executed");
    expect(container.querySelector(".bg-emerald-500")).not.toBeNull();
  });
});

describe("NotebookCellOutput — Phase 3 images/variables (#879)", () => {
  it("renders no images/variables blocks when the result has no meta trailer", () => {
    renderStep({
      callId: "c5",
      tool: "notebook_runner",
      args: { action: "run_cell", code: "print(1)" },
      status: "done",
      result: "1\n",
    });
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("table")).toBeNull();
  });

  it("renders a variables table from an inspect dump", () => {
    const trailer =
      '\n<<<FF_NB_META\n{"images":[],"variables":[{"name":"df","type":"DataFrame","repr":"<5 rows>"}]}\nFF_NB_META\n';
    renderStep({
      callId: "c6",
      tool: "notebook_runner",
      args: { action: "inspect" },
      status: "done",
      result: `1 variable(s) in scope:\n  df: DataFrame = <5 rows>${trailer}`,
    });
    const text = container.textContent ?? "";
    expect(text).toContain("df");
    expect(text).toContain("DataFrame");
    expect(text).toContain("<5 rows>");
    expect(container.querySelector("table")).not.toBeNull();
  });

  it("renders a resolved image as a data: URI once the FS read completes", async () => {
    readFile.mockResolvedValue(new Uint8Array([1, 2, 3]));
    const trailer =
      '\n<<<FF_NB_META\n{"images":[{"path":"/tmp/fig.png","mediaType":"image/png"}],"variables":[]}\nFF_NB_META\n';
    await act(async () => {
      root.render(
        <NotebookCellOutput
          step={{
            callId: "c7",
            tool: "notebook_runner",
            args: { action: "run_cell", code: "plt.plot([1])" },
            status: "done",
            result: `plotted.${trailer}`,
          }}
        />,
      );
      // Let the readFile promise chain + resulting setState settle before we
      // assert — a plain microtask await isn't guaranteed to drain every hop.
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(img?.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
    expect(readFile).toHaveBeenCalledWith("/tmp/fig.png");
  });

  it("skips an image whose file read fails, without throwing", async () => {
    readFile.mockRejectedValue(new Error("ENOENT"));
    const trailer =
      '\n<<<FF_NB_META\n{"images":[{"path":"/tmp/gone.png","mediaType":"image/png"}],"variables":[]}\nFF_NB_META\n';
    await act(async () => {
      root.render(
        <NotebookCellOutput
          step={{
            callId: "c8",
            tool: "notebook_runner",
            args: { action: "run_cell", code: "plt.plot([1])" },
            status: "done",
            result: `plotted.${trailer}`,
          }}
        />,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("plotted.");
  });
});
