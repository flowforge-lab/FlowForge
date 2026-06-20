// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ToolStepBlock } from "@/components/tool-step";
import type { ToolStep } from "@/store/chat";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => {
    root.render(ui);
  });
}

function click(el: Element | null | undefined) {
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

/** Find a button by its trimmed text content. */
function button(label: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === label,
  ) as HTMLButtonElement | undefined;
}

const awaitingStep: ToolStep = {
  callId: "c1",
  tool: "edit",
  args: { path: "README.md" },
  status: "awaiting-approval",
  safety: "write",
};

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
  vi.restoreAllMocks();
});

describe("ToolStepBlock — four-option approval (#229)", () => {
  it("'Allow once' calls onRespond(callId, true)", () => {
    const onRespond = vi.fn();
    render(
      <ToolStepBlock
        step={awaitingStep}
        onRespond={onRespond}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    click(button("Allow once"));
    expect(onRespond).toHaveBeenCalledWith("c1", true);
  });

  it("'Allow this session' calls onApproveSession(callId, tool)", () => {
    const onApproveSession = vi.fn();
    render(
      <ToolStepBlock
        step={awaitingStep}
        onRespond={vi.fn()}
        onApproveSession={onApproveSession}
        onApproveAlways={vi.fn()}
      />,
    );
    click(button("Allow this session"));
    expect(onApproveSession).toHaveBeenCalledWith("c1", "edit");
  });

  it("'Always allow' calls onApproveAlways(callId, tool)", () => {
    const onApproveAlways = vi.fn();
    render(
      <ToolStepBlock
        step={awaitingStep}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={onApproveAlways}
      />,
    );
    click(button("Always allow"));
    expect(onApproveAlways).toHaveBeenCalledWith("c1", "edit");
  });

  it("'Deny' calls onRespond(callId, false)", () => {
    const onRespond = vi.fn();
    render(
      <ToolStepBlock
        step={awaitingStep}
        onRespond={onRespond}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    click(button("Deny"));
    expect(onRespond).toHaveBeenCalledWith("c1", false);
  });

  it("hides the persistent tiers for a dangerous-safety gate (#232 allowlist exclusion)", () => {
    render(
      <ToolStepBlock
        step={{ ...awaitingStep, safety: "dangerous" }}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    expect(button("Allow once")).toBeTruthy();
    expect(button("Deny")).toBeTruthy();
    expect(button("Allow this session")).toBeUndefined();
    expect(button("Always allow")).toBeUndefined();
  });

  it("renders a 'session' badge on a session-approved resolved step", () => {
    render(
      <ToolStepBlock
        step={{ ...awaitingStep, status: "done", sessionApproved: true }}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    expect(container.textContent).toContain("session");
  });

  it("renders an 'always' badge on an always-approved resolved step", () => {
    render(
      <ToolStepBlock
        step={{ ...awaitingStep, status: "done", alwaysApproved: true }}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    expect(container.textContent).toContain("always");
  });
});
