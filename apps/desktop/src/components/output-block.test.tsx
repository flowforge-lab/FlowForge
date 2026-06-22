// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { OutputBlock, OUTPUT_FOLD_THRESHOLD } from "@/components/output-block";
import { useSplitStore } from "@/store/split";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

function button(label: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.trim().startsWith(label),
  ) as HTMLButtonElement | undefined;
}

function click(el: Element | null | undefined) {
  act(() => el?.dispatchEvent(new MouseEvent("click", { bubbles: true })));
}

const pre = () => container.querySelector("pre");

const SHORT = "short output";
const LONG = "x".repeat(OUTPUT_FOLD_THRESHOLD + 50);

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

describe("OutputBlock (#331)", () => {
  it("renders short output inline (no fold toggle)", () => {
    render(<OutputBlock output={SHORT} title="t" />);
    expect(pre()?.textContent).toBe(SHORT);
    // No collapse chevron for short output.
    expect(button("output")).toBeUndefined();
  });

  it("starts long output collapsed, then expands to the full content", () => {
    render(<OutputBlock output={LONG} title="t" />);
    // Collapsed: no <pre> yet, just the toggle.
    expect(pre()).toBeNull();
    const toggle = button("output");
    expect(toggle).toBeTruthy();

    click(toggle);
    // Expanded: full content (never truncated) in a height-capped scroll box.
    expect(pre()?.textContent).toBe(LONG);
    expect(pre()?.className).toContain("max-h-64");
    expect(pre()?.className).toContain("overflow-auto");
  });

  it("uses neutral styling by default and destructive only on error", () => {
    render(<OutputBlock output={SHORT} title="t" />);
    expect(pre()?.className).toContain("text-foreground/90");
    expect(pre()?.className).not.toContain("text-destructive");

    render(<OutputBlock output={SHORT} title="t" error />);
    expect(pre()?.className).toContain("text-destructive");
  });

  it("copies the full output to the clipboard", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<OutputBlock output={SHORT} title="t" />);
    click(button("Copy"));
    expect(writeText).toHaveBeenCalledWith(SHORT);
  });

  it("opens the output in the split panel", () => {
    render(<OutputBlock output={SHORT} title="my-tool" />);
    click(button("Split"));
    const { content } = useSplitStore.getState();
    expect(content).toEqual({ kind: "text", text: SHORT, title: "my-tool" });
  });
});
