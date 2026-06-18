// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AboutSection } from "@/components/settings/about-section";
import { useSettingsStore } from "@/store/settings";

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

function click(el: Element | null) {
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useSettingsStore.setState({ activeSection: "about", resetHandler: null });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("AboutSection", () => {
  it("View all keyboard shortcuts navigates to the Keyboard section", () => {
    render(<AboutSection />);
    const link = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("View all keyboard shortcuts"),
    );
    expect(link).toBeDefined();
    click(link ?? null);
    expect(useSettingsStore.getState().activeSection).toBe("keyboard");
  });

  it("Check for updates toasts FE-owned copy from the structured result", async () => {
    render(<AboutSection />);
    const btn = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Check for updates"),
    );
    expect(btn).toBeDefined();
    await act(async () => {
      btn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "You're on the latest version.",
    );
  });
});
