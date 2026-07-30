// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "@/lib/ipc";
import { UpdateBar } from "@/components/update-bar";
import { useExperimentalStore } from "@/store/experimental";
import { useUpdateStore } from "@/store/update";

import App from "./App";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const AVAILABLE = {
  kind: "available" as const,
  version: "9.9.9",
  currentVersion: "0.0.0-dev.1",
  notes: null,
  date: null,
};

let container: HTMLDivElement;
let root: Root | null = null;

/** Silence the boot work that isn't under test, leaving the update poll intact. */
function stubBoot() {
  vi.spyOn(ipc, "isAppReady").mockResolvedValue(true);
  vi.spyOn(ipc, "onAppReady").mockResolvedValue(() => {});
  vi.spyOn(ipc, "onAppInitError").mockResolvedValue(() => {});
  vi.spyOn(ipc, "startDevUpdateWatcher").mockResolvedValue(undefined);
  vi.spyOn(ipc, "onLocalFeedChanged").mockResolvedValue(() => {});
}

beforeEach(() => {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }),
  });
  container = document.createElement("div");
  document.body.appendChild(container);
  useExperimentalStore.setState({
    flags: { localUpdateChannel: true } as never,
  });
  useUpdateStore.setState({ status: null, installing: false });
});

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container.remove();
  vi.restoreAllMocks();
});

async function render() {
  await act(async () => {
    root = createRoot(container);
    root.render(<App />);
  });
}

describe("App — local update channel (#1158)", () => {
  it("does not install a detected update without a click", async () => {
    stubBoot();
    const refresh = vi
      .spyOn(useUpdateStore.getState(), "refresh")
      .mockImplementation(async () => {
        useUpdateStore.setState({ status: AVAILABLE });
      });
    const install = vi.spyOn(ipc, "installUpdate").mockResolvedValue();

    await render();

    // Guard against a vacuous pass: if the poll never ran, `install` would be
    // trivially uncalled and this test would prove nothing.
    expect(refresh).toHaveBeenCalled();
    expect(useUpdateStore.getState().status?.kind).toBe("available");
    expect(install).not.toHaveBeenCalled();
  });

  it("installs exactly the banner's build when the user clicks Update", async () => {
    useUpdateStore.setState({ status: AVAILABLE });
    const install = vi.spyOn(ipc, "installUpdate").mockResolvedValue();

    await act(async () => {
      root = createRoot(container);
      root.render(<UpdateBar />);
    });

    const button = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Update",
    );
    expect(button).toBeDefined();

    await act(async () => {
      button?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(install).toHaveBeenCalledTimes(1);
    // The version the bar showed, so a feed that moves mid-click is refused.
    expect(install.mock.calls[0]?.[1]).toBe("9.9.9");
  });
});
