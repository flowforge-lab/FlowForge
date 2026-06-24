// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Spinner } from "@/components/ui/spinner";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

const status = () => container.querySelector('[role="status"]');

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

describe("Spinner (#284 §3)", () => {
  it("renders an accessible spinning status with the default label", () => {
    render(<Spinner />);
    const el = status();
    expect(el).not.toBeNull();
    expect(el?.getAttribute("aria-label")).toBe("Loading");
    expect(el?.classList.contains("animate-spin")).toBe(true);
    expect(el?.classList.contains("size-3.5")).toBe(true);
  });

  it("honors the size prop and a custom aria-label", () => {
    render(<Spinner size="md" aria-label="Saving" />);
    const el = status();
    expect(el?.classList.contains("size-4")).toBe(true);
    expect(el?.getAttribute("aria-label")).toBe("Saving");
  });
});
