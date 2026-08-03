// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PreheatDroppedToast } from "@/components/preheat-dropped-toast";
import { usePreheatNoticeStore } from "@/store/preheat-notice";
import type { PhenotypePreheatDroppedEvent } from "@/bindings";

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

function notice(
  over: Partial<PhenotypePreheatDroppedEvent> = {},
): PhenotypePreheatDroppedEvent {
  return {
    phenotype: "codon",
    sessionId: "s1",
    unknown: [],
    overBudget: [],
    admittedBytes: 0,
    ...over,
  };
}

function text(): string {
  return container.querySelector('[role="status"]')?.textContent ?? "";
}

beforeEach(() => {
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  usePreheatNoticeStore.setState({ notice: null });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("PreheatDroppedToast", () => {
  it("renders nothing when there is no notice", () => {
    render(<PreheatDroppedToast />);
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("names the phenotype and reassures that the tools still work", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore
        .getState()
        .show(notice({ unknown: ["nosuchtool"] }));
    });
    expect(text()).toContain("codon");
    expect(text()).toContain("tool_search");
  });

  it("reports unknown names as a likely typo", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore.getState().show(notice({ unknown: ["web_ftech"] }));
    });
    expect(text()).toContain("No such tool: web_ftech");
    expect(text()).toContain("typo");
  });

  it("reports over-budget names separately from unknown ones", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore.getState().show(
        notice({
          unknown: ["web_ftech"],
          overBudget: ["memory_search", "web_fetch"],
        }),
      );
    });
    const body = text();
    expect(body).toContain("No such tool: web_ftech");
    expect(body).toContain("Over the preheat budget: memory_search, web_fetch");
    // The two causes have different fixes, so they must not be merged into one line.
    expect(body.indexOf("No such tool")).not.toBe(body.indexOf("Over the"));
  });

  it("omits the unknown line when nothing was unknown", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore
        .getState()
        .show(notice({ overBudget: ["web_fetch"] }));
    });
    expect(text()).not.toContain("No such tool");
    expect(text()).toContain("Over the preheat budget");
  });

  it("is sticky: does not auto-dismiss over time", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore.getState().show(notice({ unknown: ["x"] }));
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(120_000);
    });
    // The dropped tools stay dropped all session, so the only signal must persist.
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    expect(usePreheatNoticeStore.getState().notice).not.toBeNull();
  });

  it("Dismiss clears the notice immediately", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore.getState().show(notice({ unknown: ["x"] }));
    });
    click(container.querySelector('button[aria-label="Dismiss"]'));
    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(usePreheatNoticeStore.getState().notice).toBeNull();
  });

  it("a replacing notice shows the new payload, not the stale one", () => {
    render(<PreheatDroppedToast />);
    act(() => {
      usePreheatNoticeStore.getState().show(notice({ unknown: ["first"] }));
    });
    expect(text()).toContain("first");
    act(() => {
      usePreheatNoticeStore
        .getState()
        .show(notice({ phenotype: "reviewer", unknown: ["second"] }));
    });
    expect(text()).toContain("second");
    expect(text()).not.toContain("first");
    expect(text()).toContain("reviewer");
  });
});
