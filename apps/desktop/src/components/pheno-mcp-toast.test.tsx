// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PhenoMcpToast } from "@/components/pheno-mcp-toast";
import { usePhenoMcpNoticeStore } from "@/store/pheno-mcp-notice";
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

function fire(el: Element | null, type: string) {
  act(() => {
    el?.dispatchEvent(new MouseEvent(type, { bubbles: true }));
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
  usePhenoMcpNoticeStore.setState({ notice: null, seq: 0 });
  useSettingsStore.setState({ open: false, activeSection: "appearance" });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("PhenoMcpToast", () => {
  it("renders nothing when there is no notice", () => {
    render(<PhenoMcpToast />);
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("shows the full body copy for the active notice", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "codon needs the codegraph MCP server, which is not available. Its grep/glob fallbacks still work — add or start it in MCP settings.",
    );
  });

  it("auto-dismisses after the timeout", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();

    act(() => {
      vi.advanceTimersByTime(12_000);
    });
    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(usePhenoMcpNoticeStore.getState().notice).toBeNull();
  });

  it("Dismiss clears the notice immediately", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    click(container.querySelector('button[aria-label="Dismiss"]'));
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("does not auto-dismiss while the pointer is over it", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    const card = container.querySelector('[role="status"]');
    // React synthesizes onMouseEnter/Leave from native mouseover/mouseout.
    fire(card, "mouseover");
    act(() => {
      vi.advanceTimersByTime(12_000);
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    expect(usePhenoMcpNoticeStore.getState().notice).not.toBeNull();
  });

  it("re-arms a fresh countdown after the pointer leaves", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    const card = container.querySelector('[role="status"]');
    fire(card, "mouseover");
    act(() => {
      vi.advanceTimersByTime(12_000);
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();

    fire(card, "mouseout");
    // Leaving re-arms the full interval, so it survives a partial advance...
    act(() => {
      vi.advanceTimersByTime(11_000);
    });
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    // ...and dismisses once the fresh full interval elapses.
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(container.querySelector('[role="status"]')).toBeNull();
  });

  it("Open MCP settings opens the MCP section and dismisses", () => {
    render(<PhenoMcpToast />);
    act(() => {
      usePhenoMcpNoticeStore
        .getState()
        .show({ phenotype: "codon", servers: ["codegraph"] });
    });
    click(findButton("Open MCP settings") ?? null);
    const settings = useSettingsStore.getState();
    expect(settings.open).toBe(true);
    expect(settings.activeSection).toBe("mcp");
    expect(usePhenoMcpNoticeStore.getState().notice).toBeNull();
  });
});
