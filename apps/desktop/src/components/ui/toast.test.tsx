// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Toast, ToastViewport } from "@/components/ui/toast";

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

describe("Toast (#284 §2)", () => {
  it("announces its content via role=status inside the viewport", () => {
    render(
      <ToastViewport>
        <Toast>Heads up</Toast>
      </ToastViewport>,
    );
    expect(
      container.querySelector('[data-slot="toast-viewport"]'),
    ).not.toBeNull();
    const toast = container.querySelector('[role="status"]');
    expect(toast?.getAttribute("aria-live")).toBe("polite");
    expect(toast?.textContent).toContain("Heads up");
  });
});
