// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AboutSection } from "@/components/settings/about-section";
import { ipc } from "@/lib/ipc";
import { useSettingsStore } from "@/store/settings";
import { useUpdateStore } from "@/store/update";

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
  useUpdateStore.setState({ status: null, installing: false });
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

  function updateNowButton(): HTMLButtonElement | undefined {
    return [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Update now"),
    ) as HTMLButtonElement | undefined;
  }

  it("shows 'Update now' only when an update is available", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<AboutSection />);
    const btn = updateNowButton();
    expect(btn).toBeDefined();
    expect(btn?.textContent).toContain("9.9.9");
  });

  it("does not show 'Update now' when up to date", () => {
    useUpdateStore.setState({
      status: { kind: "upToDate", version: "0.1.0" },
    });
    render(<AboutSection />);
    expect(updateNowButton()).toBeUndefined();
  });

  it("'Update now' calls installUpdate once", async () => {
    const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<AboutSection />);
    await act(async () => {
      updateNowButton()?.dispatchEvent(
        new MouseEvent("click", { bubbles: true }),
      );
    });
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("disables 'Update now' and shows a spinner while installing", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
    });
    render(<AboutSection />);
    const btn = updateNowButton();
    expect(btn).toBeDefined();
    expect(btn?.disabled).toBe(true);
    expect(btn?.querySelector(".animate-spin")).not.toBeNull();
  });
});
