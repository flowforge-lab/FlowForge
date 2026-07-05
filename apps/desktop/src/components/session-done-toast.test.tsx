// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionDoneToast } from "@/components/session-done-toast";
import { useChatStore } from "@/store/chat";
import { useSessionDoneToastStore } from "@/store/session-done-toast";

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

beforeEach(() => {
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useSessionDoneToastStore.setState({ toasts: [] });
  // Isolate the component from IPC: stub the navigation actions "View" calls.
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

describe("SessionDoneToast (#703)", () => {
  it("renders nothing when the queue is empty", () => {
    render(<SessionDoneToast />);
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("renders a card per queued completion", () => {
    render(<SessionDoneToast />);
    act(() => {
      useSessionDoneToastStore.getState().push("s1", "Parser cleanup");
      useSessionDoneToastStore.getState().push("s2", "Docs pass");
    });
    const cards = container.querySelectorAll('[role="status"]');
    expect(cards).toHaveLength(2);
    expect(container.textContent).toContain("Parser cleanup");
    expect(container.textContent).toContain("Docs pass");
  });

  it("'View' navigates to the session and dismisses its toast", () => {
    render(<SessionDoneToast />);
    act(() => {
      useSessionDoneToastStore.getState().push("s1", "Parser cleanup");
    });
    click(findButton("View"));
    expect(useChatStore.getState().activeSessionId).toBe("s1");
    expect(useSessionDoneToastStore.getState().toasts).toHaveLength(0);
  });

  it("auto-dismisses after 10s", () => {
    render(<SessionDoneToast />);
    act(() => {
      useSessionDoneToastStore.getState().push("s1", "Parser cleanup");
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(useSessionDoneToastStore.getState().toasts).toHaveLength(0);
  });
});
