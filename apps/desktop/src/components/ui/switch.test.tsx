// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Switch } from "@/components/ui/switch";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

const sw = () => container.querySelector('[role="switch"]');

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

describe("Switch (#284)", () => {
  it("exposes role=switch and reflects the checked state via aria-checked", () => {
    render(<Switch checked={false} />);
    expect(sw()).not.toBeNull();
    expect(sw()?.getAttribute("aria-checked")).toBe("false");

    render(<Switch checked />);
    expect(sw()?.getAttribute("aria-checked")).toBe("true");
  });

  it("reflects the disabled state", () => {
    render(<Switch checked={false} disabled />);
    expect(sw()?.hasAttribute("disabled")).toBe(true);
    expect(sw()?.getAttribute("data-disabled")).not.toBeNull();
  });

  it("merges a passed className with the base classes", () => {
    render(<Switch checked={false} className="ml-2" />);
    expect(sw()?.className).toContain("ml-2");
    // Base classes survive the merge.
    expect(sw()?.className).toContain("rounded-full");
  });
});
