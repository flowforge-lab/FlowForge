// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Progress } from "@/components/ui/progress";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

const bar = () => container.querySelector('[role="progressbar"]');
const indicator = () =>
  container.querySelector<HTMLElement>('[data-slot="progress-indicator"]');

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

describe("Progress (#284)", () => {
  it("exposes role=progressbar with aria-valuenow and a matching indicator offset", () => {
    render(<Progress value={42} />);
    expect(bar()).not.toBeNull();
    expect(bar()?.getAttribute("aria-valuenow")).toBe("42");
    // The fill is translated left by (100 - value)%.
    expect(indicator()?.style.transform).toBe("translateX(-58%)");
  });

  it("clamps both the indicator offset and aria-valuenow for out-of-range values", () => {
    render(<Progress value={150} />);
    expect(indicator()?.style.transform).toBe("translateX(-0%)");
    expect(bar()?.getAttribute("aria-valuenow")).toBe("100");
    render(<Progress value={-10} />);
    expect(indicator()?.style.transform).toBe("translateX(-100%)");
    expect(bar()?.getAttribute("aria-valuenow")).toBe("0");
  });

  it("renders an empty track with no aria-valuenow when indeterminate", () => {
    render(<Progress />);
    expect(bar()).not.toBeNull();
    expect(indicator()?.style.transform).toBe("translateX(-100%)");
    // Indeterminate: Radix omits aria-valuenow entirely.
    expect(bar()?.hasAttribute("aria-valuenow")).toBe(false);
  });
});
