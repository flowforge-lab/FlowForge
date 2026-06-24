// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ErrorState } from "@/components/ui/error-state";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

const alert = () => container.querySelector('[role="alert"]');
const retryButton = () =>
  [...container.querySelectorAll("button")].find((el) =>
    el.textContent?.includes("Try again"),
  );

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

describe("ErrorState (#284 §3)", () => {
  it("announces the message via role=alert", () => {
    render(<ErrorState message="Couldn’t reach the marketplace." />);
    expect(alert()?.textContent).toContain("Couldn’t reach the marketplace.");
  });

  it("renders no retry button when onRetry is omitted", () => {
    render(<ErrorState message="Failed." />);
    expect(retryButton()).toBeUndefined();
  });

  it("renders the retry button and fires the callback on click", () => {
    const onRetry = vi.fn();
    render(<ErrorState message="Failed." onRetry={onRetry} />);
    const btn = retryButton();
    expect(btn).toBeDefined();
    act(() => {
      btn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onRetry).toHaveBeenCalledTimes(1);
  });
});
