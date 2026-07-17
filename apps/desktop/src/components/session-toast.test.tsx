// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionToasts } from "@/components/session-toast";
import { useChatStore } from "@/store/chat";
import { useSessionToastStore, type ToastKind } from "@/store/session-toast";

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

function click(el: Element | null | undefined) {
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function findButton(label: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find((el) =>
    el.textContent?.includes(label),
  ) as HTMLButtonElement | undefined;
}

function push(kind: ToastKind, sessionId: string, title: string) {
  act(() => {
    useSessionToastStore.getState().push({ kind, sessionId, title });
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useSessionToastStore.setState({ toasts: [] });
  // Isolate the component from IPC: stub the navigation actions the action calls.
  useChatStore.setState({
    activeSessionId: null,
    selectSession: async (id: string) => {
      useChatStore.setState({ activeSessionId: id });
    },
    loadSession: async () => {},
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("SessionToasts (#703, #994)", () => {
  it("renders nothing when the queue is empty", () => {
    render(<SessionToasts />);
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("renders a card per queued toast, across kinds", () => {
    render(<SessionToasts />);
    push("done", "s1", "Parser cleanup");
    push("error", "s2", "Docs pass");
    const cards = container.querySelectorAll('[role="status"]');
    expect(cards).toHaveLength(2);
    expect(container.textContent).toContain("Parser cleanup");
    expect(container.textContent).toContain("Docs pass");
  });

  it("shows the right label + action per kind", () => {
    render(<SessionToasts />);
    push("done", "s1", "A");
    expect(container.textContent).toContain("Finished");
    expect(findButton("View")).toBeTruthy();

    useSessionToastStore.setState({ toasts: [] });
    push("approval", "s2", "B");
    expect(container.textContent).toContain("Needs your approval");
    expect(findButton("Review")).toBeTruthy();

    useSessionToastStore.setState({ toasts: [] });
    push("error", "s3", "C");
    expect(container.textContent).toContain("Failed");

    useSessionToastStore.setState({ toasts: [] });
    push("stopped", "s4", "D");
    expect(findButton("Continue")).toBeTruthy();
  });

  it("the action navigates to the session and dismisses its toast", () => {
    render(<SessionToasts />);
    push("approval", "s1", "Parser cleanup");
    click(findButton("Review"));
    expect(useChatStore.getState().activeSessionId).toBe("s1");
    expect(useSessionToastStore.getState().toasts).toHaveLength(0);
  });

  it("done/stopped auto-dismiss after 10s", () => {
    render(<SessionToasts />);
    push("done", "s1", "A");
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    act(() => vi.advanceTimersByTime(10_000));
    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(useSessionToastStore.getState().toasts).toHaveLength(0);
  });

  it("approval/error are sticky — no auto-dismiss", () => {
    render(<SessionToasts />);
    push("approval", "s1", "A");
    push("error", "s2", "B");
    act(() => vi.advanceTimersByTime(60_000));
    expect(useSessionToastStore.getState().toasts).toHaveLength(2);
    expect(container.querySelectorAll('[role="status"]')).toHaveLength(2);
  });

  it("dedups by (sessionId, kind) — a repeat replaces, not stacks", () => {
    render(<SessionToasts />);
    push("approval", "s1", "First");
    push("approval", "s1", "Second");
    const toasts = useSessionToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].title).toBe("Second");
    // A different kind for the same session is a separate card.
    push("error", "s1", "Boom");
    expect(useSessionToastStore.getState().toasts).toHaveLength(2);
  });

  it("dismissBySession drops every kind for a session", () => {
    render(<SessionToasts />);
    push("approval", "s1", "A");
    push("error", "s1", "B");
    push("done", "s2", "C");
    act(() => useSessionToastStore.getState().dismissBySession("s1"));
    const toasts = useSessionToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].sessionId).toBe("s2");
  });
});
