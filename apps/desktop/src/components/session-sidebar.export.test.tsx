// @vitest-environment jsdom

import type { ComponentType, ReactNode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionMenuItems } from "@/components/session-sidebar";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

// Plain stand-ins for the radix menu parts, so we can render and click the items
// without a live menu. `Item` maps onSelect → onClick (matching how radix fires it).
const Item: ComponentType<{
  onSelect?: (e: Event) => void;
  children?: ReactNode;
}> = ({ onSelect, children }) => (
  <button type="button" onClick={() => onSelect?.(new Event("select"))}>
    {children}
  </button>
);
const Pass: ComponentType<{ children?: ReactNode }> = ({ children }) => (
  <div>{children}</div>
);
const STUB_PARTS = {
  Item,
  Sub: Pass,
  SubTrigger: Pass,
  SubContent: Pass,
  Separator: () => <hr />,
};

let container: HTMLDivElement;
let root: Root;

function clickByText(text: string) {
  const el = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function noop() {}

function renderMenu(onExport: (f: "markdown" | "json") => void) {
  act(() => {
    root.render(
      <SessionMenuItems
        parts={STUB_PARTS}
        atCap={false}
        pinned={false}
        dismissed={false}
        onOpen={noop}
        onOpenSplit={noop}
        onTogglePin={noop}
        onDismissToggle={noop}
        onFork={noop}
        onRename={noop}
        onExport={onExport}
        onDelete={noop}
      />,
    );
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

describe("SessionMenuItems export (#278)", () => {
  it("renders both export options", () => {
    renderMenu(noop);
    const labels = [...container.querySelectorAll("button")].map((b) =>
      b.textContent?.trim(),
    );
    expect(labels).toContain("Markdown (.md)");
    expect(labels).toContain("JSON (.json)");
  });

  it("invokes onExport with the chosen format", () => {
    const onExport = vi.fn();
    renderMenu(onExport);
    clickByText("Markdown (.md)");
    expect(onExport).toHaveBeenCalledWith("markdown");
    clickByText("JSON (.json)");
    expect(onExport).toHaveBeenCalledWith("json");
  });
});
