// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { NotebookCellOutput } from "@/components/notebook-cell-output";
import type { ToolStep } from "@/store/chat";

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
