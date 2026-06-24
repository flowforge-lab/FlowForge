// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { EmptyState } from "@/components/ui/empty-state";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
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

describe("EmptyState (#284 §3)", () => {
  it("renders the title and optional hint", () => {
    render(<EmptyState title="No sessions yet" hint="Start a new chat" />);
    expect(container.textContent).toContain("No sessions yet");
    expect(container.textContent).toContain("Start a new chat");
  });

  it("omits the hint and icon when not provided", () => {
    render(<EmptyState title="Nothing here" />);
    expect(container.querySelectorAll("p")).toHaveLength(1);
    expect(container.querySelector("svg")).toBeNull();
  });

  it("renders the icon when provided", () => {
    function Dot({ className }: { className?: string }) {
      return <svg className={className} data-testid="dot" />;
    }
    render(<EmptyState icon={Dot} title="Empty" />);
    expect(container.querySelector('[data-testid="dot"]')).not.toBeNull();
  });
});
