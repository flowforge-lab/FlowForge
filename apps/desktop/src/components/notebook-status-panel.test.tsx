// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotebookStatusPanel } from "@/components/notebook-status-panel";
import type { NotebookKernelState } from "@/lib/notebook-kernel-state";
import {
  NOTEBOOK_POLL_DEFAULT_MS,
  useExperimentalStore,
} from "@/store/experimental";
import { useNotebookStore } from "@/store/notebook";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function snapshot(
  partial: Partial<NotebookKernelState> & { sessionId: string },
): NotebookKernelState {
  return {
    hasKernel: false,
    state: null,
    kernelId: null,
    pid: null,
    executionCount: 0,
    raw: "",
    ...partial,
  };
}

function renderPanel(sessionId: string) {
  act(() => {
    root.render(<NotebookStatusPanel sessionId={sessionId} />);
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useNotebookStore.setState({ bySession: {}, ipcUnavailable: false });
  useExperimentalStore.setState({
    notebookPollIntervalMs: NOTEBOOK_POLL_DEFAULT_MS,
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("NotebookStatusPanel (#871 FE-1)", () => {
  it("renders nothing before the first hydrate resolves", () => {
    // Snapshot is undefined -> panel must self-hide (no flicker, no error).
    renderPanel("s1");
    expect(container.textContent ?? "").toBe("");
  });

  it("renders the 'no kernel' row when the snapshot is null", () => {
    act(() => {
      useNotebookStore.setState({
        bySession: { s1: snapshot({ sessionId: "s1", hasKernel: false }) },
      });
    });
    renderPanel("s1");
    expect(container.textContent).toContain("No kernel for this session");
    // No Stop button on the no-kernel state.
    expect(
      container.querySelector("button[title='Stop the kernel']"),
    ).toBeNull();
  });

  it("renders a live pill + kernel id + execution count + Stop button when running", () => {
    act(() => {
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "running",
            kernelId: "kernel-aaaa",
            pid: 4242,
            executionCount: 7,
            raw: "kernel kernel-aaaa — running; pid=4242; cells executed=7",
          }),
        },
      });
    });
    renderPanel("s1");

    const text = container.textContent ?? "";
    expect(text).toContain("kernel running");
    expect(text).toContain("kernel-aaaa");
    expect(text).toContain("7 cells executed");
    expect(text).toContain("polling");

    // The emerald live dot is present; no destructive dot.
    expect(container.querySelector(".bg-emerald-500")).not.toBeNull();
    expect(container.querySelector(".bg-destructive")).toBeNull();
    // Stop button rendered (Stop label).
    expect(
      container.querySelector("button[title='Stop the kernel']"),
    ).not.toBeNull();
  });

  it("renders a dead pill + restart hint, no Stop button, no polling label", () => {
    act(() => {
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "dead",
            kernelId: "kernel-bbbb",
            executionCount: 4,
            raw: "kernel kernel-bbbb — dead; pid=1; cells executed=4",
          }),
        },
      });
    });
    renderPanel("s1");

    const text = container.textContent ?? "";
    expect(text).toContain("kernel dead");
    expect(text).toContain("kernel-bbbb");
    // The expanded hint advises the agent to call start.
    expect(text).toContain("notebook_runner start");
    // No Stop button + no polling activity.
    expect(
      container.querySelector("button[title='Stop the kernel']"),
    ).toBeNull();
    expect(text).not.toContain("polling");
  });

  it("clicking Stop invokes notebookStop + refreshes via the store", async () => {
    act(() => {
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "running",
            kernelId: "kernel-aaaa",
            executionCount: 2,
          }),
        },
      });
    });
    renderPanel("s1");

    // Replace the store's stop with a spy so we can assert the click
    // delegation without reaching into the real ipc (whose mock rejection
    // would break the test).
    const stopSpy = vi.fn(async () => {
      // Simulate the post-stop refresh: the real backend removes the kernel
      // entirely on Stop, so the session collapses to "no kernel" — not a
      // `state: "dead"` tombstone (that's reserved for a kernel that died on
      // its own).
      act(() => {
        useNotebookStore.setState({
          bySession: {
            s1: snapshot({ sessionId: "s1", hasKernel: false }),
          },
        });
      });
    });
    act(() => {
      useNotebookStore.setState((s) => ({ ...s, stop: stopSpy }) as never);
    });

    const stopBtn = container.querySelector<HTMLButtonElement>(
      "button[title='Stop the kernel']",
    );
    expect(stopBtn).not.toBeNull();
    await act(async () => {
      stopBtn?.click();
    });
    expect(stopSpy).toHaveBeenCalledWith("s1");
  });

  it("polls notebook_status while running, using the experimental cadence", async () => {
    vi.useFakeTimers();
    act(() => {
      useExperimentalStore.setState({ notebookPollIntervalMs: 1000 });
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "running",
            kernelId: "kernel-aaaa",
            executionCount: 0,
          }),
        },
      });
    });
    renderPanel("s1");

    const refreshSpy = vi.fn();
    act(() => {
      useNotebookStore.setState(
        (s) => ({ ...s, refresh: refreshSpy }) as never,
      );
    });

    // After ~2.5 ticks the spy should have been called twice.
    await act(async () => {
      vi.advanceTimersByTime(2500);
    });
    expect(refreshSpy).toHaveBeenCalledWith("s1");
    expect(refreshSpy.mock.calls.length).toBeGreaterThanOrEqual(2);

    // Once we transition out of running, polling must stop.
    act(() => {
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "dead",
            kernelId: "kernel-aaaa",
            executionCount: 5,
          }),
        },
      });
      refreshSpy.mockClear();
    });
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });
    expect(refreshSpy).not.toHaveBeenCalled();
    vi.useRealTimers();
  });
});
