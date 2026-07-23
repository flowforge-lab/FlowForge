// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "@/lib/ipc";
import type { ObserverInfo } from "@/bindings";
import { ObserverPanel } from "@/components/observer-panel";
import { useObserversStore } from "@/store/observers";

function observer(
  partial: Partial<ObserverInfo> & { id: number },
): ObserverInfo {
  return {
    label: `obs-${partial.id}`,
    kind: "file",
    target: "src/lib.rs",
    startedAt: "2026-07-23T00:00:00Z",
    ...partial,
  };
}

beforeEach(() => {
  useObserversStore.setState({ bySession: {} });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ObserverPanel (#1038)", () => {
  it("self-hides when the session has no observers", async () => {
    vi.spyOn(ipc, "listObservers").mockResolvedValue([]);
    const { container } = render(<ObserverPanel sessionId="s1" />);
    // Let the mount-load resolve; the empty list keeps the panel hidden.
    await Promise.resolve();
    expect(container.textContent ?? "").toBe("");
  });

  it("loads on mount and lists each observer with its target + kind hint", async () => {
    vi.spyOn(ipc, "listObservers").mockResolvedValue([
      observer({ id: 1, kind: "file", target: "src/lib.rs" }),
      observer({ id: 2, kind: "http", target: "localhost:3000/health" }),
    ]);

    render(<ObserverPanel sessionId="s1" />);

    expect(await screen.findByText("Observers (2)")).toBeTruthy();
    expect(screen.getByText("src/lib.rs")).toBeTruthy();
    expect(screen.getByText("localhost:3000/health")).toBeTruthy();
    // Coarse kind-based hints.
    expect(screen.getByText("file changes")).toBeTruthy();
    expect(screen.getByText("polling")).toBeTruthy();
  });

  it("stops an observer via its [×] button", async () => {
    vi.spyOn(ipc, "listObservers")
      .mockResolvedValueOnce([observer({ id: 7, label: "lib.rs" })])
      .mockResolvedValue([]);
    const stopSpy = vi.spyOn(ipc, "stopObserver").mockResolvedValue();

    render(<ObserverPanel sessionId="s1" />);

    const stopBtn = await screen.findByLabelText("Stop observer lib.rs");
    fireEvent.click(stopBtn);

    expect(stopSpy).toHaveBeenCalledWith(7, "s1");
  });
});
