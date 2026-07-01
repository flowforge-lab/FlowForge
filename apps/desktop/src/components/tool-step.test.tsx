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

/** Set a controlled input/textarea's value the way React expects, then fire the
 *  matching event so onChange runs. */
function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto =
    el instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  act(() => {
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function keyDown(el: Element, key: string) {
  act(() => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  });
}

describe("ToolStepBlock — ask_user secret variant (#562)", () => {
  const askStep = (over: Partial<ToolStep> = {}): ToolStep => ({
    callId: "c1",
    tool: "ask_user",
    args: { question: "Enter your sudo password:" },
    status: "awaiting-answer",
    question: "Enter your sudo password:",
    ...over,
  });

  it("renders a masked password input (not a textarea) when secret", () => {
    render(
      <ToolStepBlock
        step={askStep({ secret: true })}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
        onAnswer={vi.fn()}
      />,
    );
    expect(container.querySelector('input[type="password"]')).toBeTruthy();
    expect(container.querySelector("textarea")).toBeNull();
  });

  it("renders the plain textarea when not secret", () => {
    render(
      <ToolStepBlock
        step={askStep()}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
        onAnswer={vi.fn()}
      />,
    );
    expect(container.querySelector("textarea")).toBeTruthy();
    expect(container.querySelector('input[type="password"]')).toBeNull();
  });

  it("Enter submits the secret via onAnswer and clears the field", () => {
    const onAnswer = vi.fn();
    render(
      <ToolStepBlock
        step={askStep({ secret: true })}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
        onAnswer={onAnswer}
      />,
    );
    const input = container.querySelector(
      'input[type="password"]',
    ) as HTMLInputElement;
    type(input, "hunter2");
    keyDown(input, "Enter");
    expect(onAnswer).toHaveBeenCalledWith("c1", "hunter2");
    expect(input.value).toBe("");
  });
});

describe("ToolStepBlock — output (#331)", () => {
  function doneStep(over: Partial<ToolStep> = {}): ToolStep {
    return {
      callId: "c1",
      tool: "bash",
      args: {},
      status: "done",
      result: "all good",
      ...over,
    };
  }

  /** Expand the collapsed-by-default step to reveal its body. */
  function expandStep() {
    click(container.querySelector("button"));
  }

  it("renders settled output neutral (not red)", () => {
    render(
      <ToolStepBlock
        step={doneStep()}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    expandStep();
    // The OutputBlock <pre> (max-h-64), not the args <pre> above it.
    const pre = container.querySelector("pre.max-h-64");
    expect(pre?.textContent).toBe("all good");
    expect(pre?.className).toContain("text-foreground/90");
    expect(pre?.className).not.toContain("text-destructive");
  });

  it("colors a failed step's output destructive", () => {
    render(
      <ToolStepBlock
        step={doneStep({ status: "error", result: "boom" })}
        onRespond={vi.fn()}
        onApproveSession={vi.fn()}
        onApproveAlways={vi.fn()}
      />,
    );
    expandStep();
    expect(container.querySelector("pre.max-h-64")?.className).toContain(
      "text-destructive",
    );
  });
});
