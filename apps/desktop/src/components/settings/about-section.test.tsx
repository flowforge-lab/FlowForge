// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AboutSection } from "@/components/settings/about-section";
import { ipc } from "@/lib/ipc";
import {
  EXPERIMENTAL_DEFAULTS,
  useExperimentalStore,
} from "@/store/experimental";
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
  useUpdateStore.setState({ status: null, installing: false, progress: null });
  useExperimentalStore.setState({ flags: { ...EXPERIMENTAL_DEFAULTS } });
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

  // About renders inside the settings ScrollArea, so a toast placed inline at the end
  // of the section is clipped below the fold — which made every action here look dead
  // (nothing visibly happened on Check for updates / What's New / Quick Setup). The
  // toast must sit in the app's fixed viewport, not in the scrolling flow.
  it("renders toasts in the fixed viewport, not inline below the fold", async () => {
    render(<AboutSection />);
    const btn = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Quick Setup"),
    );
    await act(async () => {
      btn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const toast = container.querySelector('[data-slot="toast"]');
    expect(toast?.textContent).toContain("Quick Setup wizard");
    const viewport = container.querySelector('[data-slot="toast-viewport"]');
    expect(viewport).not.toBeNull();
    expect(toast?.closest('[data-slot="toast-viewport"]')).toBe(viewport);
    // `fixed` is what escapes the scroll container; a regression to a static/inline
    // element would put it back under the fold.
    expect(viewport?.className).toContain("fixed");
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

  it("renders a determinate progress bar while downloading with a known total (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
      progress: { downloaded: 256, total: 1024 },
    });
    render(<AboutSection />);
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute("aria-valuenow")).toBe("25");
  });

  it("renders an indeterminate bar while downloading without a total (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
      progress: { downloaded: 999, total: null },
    });
    render(<AboutSection />);
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    // No content length -> indeterminate: no aria-valuenow, pulsing track.
    expect(bar?.getAttribute("aria-valuenow")).toBeNull();
    expect(bar?.classList.contains("animate-pulse")).toBe(true);
  });

  it("renders no progress bar when not installing (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: false,
    });
    render(<AboutSection />);
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  // Tiered install UX (#1034): an older local build is offered here and ONLY here,
  // and only after an explicit downgrade confirmation.
  describe("older build (#1034)", () => {
    const OLDER = {
      kind: "olderAvailable",
      version: "0.0.0-dev.1700000000",
      notes: "Local dev build abc1234 (2023-11-14T22:13:20+00:00).",
    } as const;

    // The dialog renders in a portal, outside the component's container.
    function dialogButton(label: string): HTMLButtonElement | undefined {
      return [...document.body.querySelectorAll("button")].find(
        (el) => el.textContent?.trim() === label,
      ) as HTMLButtonElement | undefined;
    }

    function installOlderRow(): HTMLButtonElement | undefined {
      return [...container.querySelectorAll("button")].find((el) =>
        el.textContent?.includes("Install older build"),
      ) as HTMLButtonElement | undefined;
    }

    it("shows the older build with its identity instead of 'Update now'", () => {
      useUpdateStore.setState({ status: OLDER });
      render(<AboutSection />);
      expect(updateNowButton()).toBeUndefined();
      expect(installOlderRow()?.textContent).toContain("0.0.0-dev.1700000000");
      // The build identity from the feed lets a dev see which commit this is.
      expect(container.textContent).toContain("abc1234");
      expect(container.textContent).toContain("Older than the running build");
    });

    it("clicking the row asks for confirmation instead of installing", () => {
      const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
      useUpdateStore.setState({ status: OLDER });
      render(<AboutSection />);
      click(installOlderRow() ?? null);
      expect(document.body.textContent).toContain("Install an older build?");
      expect(spy).not.toHaveBeenCalled();
    });

    it("cancelling installs nothing", () => {
      const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
      useUpdateStore.setState({ status: OLDER });
      render(<AboutSection />);
      click(installOlderRow() ?? null);
      click(dialogButton("Cancel") ?? null);
      expect(spy).not.toHaveBeenCalled();
    });

    it("confirming installs with the downgrade opt-in", async () => {
      const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
      useExperimentalStore.getState().setFlag("localUpdateChannel", true);
      useUpdateStore.setState({ status: OLDER });
      render(<AboutSection />);
      click(installOlderRow() ?? null);
      await act(async () => {
        dialogButton("Install older build")?.dispatchEvent(
          new MouseEvent("click", { bubbles: true }),
        );
      });
      // The confirmed version travels with the install, so a feed that moved after
      // the dialog opened is refused by the backend rather than installed (#1034).
      expect(spy).toHaveBeenCalledWith("local", OLDER.version, true);
    });

    it("surfaces the backend's refusal when the feed moved (#1034)", async () => {
      vi.spyOn(ipc, "installUpdate").mockRejectedValue(
        new Error("update feed moved: you confirmed 0.0.0-dev.1700000000"),
      );
      vi.spyOn(ipc, "checkForUpdates").mockResolvedValue({
        kind: "upToDate",
        version: "0.1.0",
      });
      useUpdateStore.setState({ status: OLDER });
      render(<AboutSection />);
      click(installOlderRow() ?? null);
      await act(async () => {
        dialogButton("Install older build")?.dispatchEvent(
          new MouseEvent("click", { bubbles: true }),
        );
      });
      expect(container.querySelector('[role="status"]')?.textContent).toContain(
        "feed moved",
      );
    });
  });

  it("hides the Developer group by default (devTools flag off)", () => {
    render(<AboutSection />);
    const btn = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Run sidecar smoke-test"),
    );
    expect(btn).toBeUndefined();
  });

  it("'Run sidecar smoke-test' invokes runSidecarTurn and toasts the event count", async () => {
    useExperimentalStore.getState().setFlag("devTools", true);
    const spy = vi
      .spyOn(ipc, "runSidecarTurn")
      .mockResolvedValue({ session_id: "abc-123", events: 5 });
    render(<AboutSection />);
    const btn = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Run sidecar smoke-test"),
    );
    expect(btn).toBeDefined();
    await act(async () => {
      btn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(spy).toHaveBeenCalledWith("hello");
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "5 event(s)",
    );
  });
});
