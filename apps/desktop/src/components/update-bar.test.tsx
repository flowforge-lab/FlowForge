// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { UpdateBar } from "@/components/update-bar";
import { ipc } from "@/lib/ipc";
import { useUpdateStore } from "@/store/update";
import { useExperimentalStore } from "@/store/experimental";

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
  useUpdateStore.setState({
    status: null,
    installing: false,
    dismissed: false,
    progress: null,
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  useExperimentalStore.getState().resetExperimental();
  vi.restoreAllMocks();
});

describe("UpdateBar (#565)", () => {
  it("shows when status is available", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<UpdateBar />);
    expect(container.querySelector(".update-bar")).not.toBeNull();
  });

  it("does not show when up to date", () => {
    useUpdateStore.setState({
      status: { kind: "upToDate", version: "0.1.0" },
    });
    render(<UpdateBar />);
    expect(container.querySelector(".update-bar")).toBeNull();
  });

  it("does not show when status is null", () => {
    render(<UpdateBar />);
    expect(container.querySelector(".update-bar")).toBeNull();
  });

  it("never banners an older build (#1034)", () => {
    // A downgrade must not interrupt: it belongs in Settings → About, behind a
    // confirmation. This is the whole "no proactive banner" acceptance criterion.
    useUpdateStore.setState({
      status: {
        kind: "olderAvailable",
        version: "0.0.0-dev.1700000000",
        notes: "Local dev build abc1234.",
      },
    });
    render(<UpdateBar />);
    expect(container.querySelector(".update-bar")).toBeNull();
  });

  it("names the active channel on the local dogfood feed (#1034)", () => {
    useExperimentalStore.setState((s) => ({
      flags: { ...s.flags, localUpdateChannel: true },
    }));
    useUpdateStore.setState({
      status: {
        kind: "available",
        version: "0.0.0-dev.1800000000",
        notes: null,
      },
    });
    render(<UpdateBar />);
    expect(container.textContent).toContain("local dev channel");
  });

  it("omits the channel tag on the github feed (#1034)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<UpdateBar />);
    expect(container.textContent).not.toContain("local dev channel");
  });

  it("Update button calls install() once", async () => {
    const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<UpdateBar />);
    const updateBtn = [...container.querySelectorAll("button")].find(
      (el) => el.textContent?.trim() === "Update",
    );
    await act(async () => {
      updateBtn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("dismiss hides the bar", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<UpdateBar />);
    expect(container.querySelector(".update-bar")).not.toBeNull();
    const dismissBtn = [...container.querySelectorAll("button")].find((el) =>
      el.getAttribute("aria-label")?.includes("Dismiss"),
    );
    click(dismissBtn ?? null);
    expect(container.querySelector(".update-bar")).toBeNull();
  });

  it("dismissed bar reappears on fresh available status", async () => {
    vi.spyOn(ipc, "checkForUpdates").mockResolvedValue({
      kind: "available",
      version: "10.0.0",
      notes: null,
    });
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
    });
    render(<UpdateBar />);
    // Dismiss it
    const dismissBtn = [...container.querySelectorAll("button")].find((el) =>
      el.getAttribute("aria-label")?.includes("Dismiss"),
    );
    click(dismissBtn ?? null);
    expect(container.querySelector(".update-bar")).toBeNull();
    // A fresh poll (`refresh`) clears `dismissed` so a still-available update
    // resurfaces the bar — the issue's "reappears on the next poll" requirement.
    await act(async () => {
      await useUpdateStore.getState().refresh("github");
    });
    expect(container.querySelector(".update-bar")).not.toBeNull();
  });

  it("shows spinner and disables button while installing", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
    });
    render(<UpdateBar />);
    const updateBtn = [...container.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("Updating"),
    );
    expect(updateBtn).not.toBeNull();
    expect(updateBtn?.disabled).toBe(true);
  });

  it("renders a determinate progress bar while installing with a known total (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
      progress: { downloaded: 25, total: 100 },
    });
    render(<UpdateBar />);
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute("aria-valuenow")).toBe("25");
  });

  it("renders an indeterminate progress bar when total is unknown (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: true,
      progress: { downloaded: 1234, total: null },
    });
    render(<UpdateBar />);
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute("aria-valuenow")).toBeNull();
  });

  it("shows no progress bar when not installing (#566)", () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      installing: false,
    });
    render(<UpdateBar />);
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });
});
