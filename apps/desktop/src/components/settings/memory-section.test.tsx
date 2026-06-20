// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemorySection } from "@/components/settings/memory-section";
import { useMemoryStore } from "@/store/memory";
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

function click(el: Element | null | undefined) {
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function banner(): HTMLElement | null {
  return container.querySelector('[role="status"]');
}

function seed(flushCount: number, lastFlushWrites = 0) {
  useMemoryStore.setState({
    files: [],
    overview: null,
    curatedBody: null,
    journalBodies: {},
    query: "",
    loading: false,
    error: null,
    flushCount,
    lastFlushWrites,
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
  useSettingsStore.setState({ activeSection: "memory", resetHandler: null });
  seed(0);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("MemorySection — flush provenance banner (#283)", () => {
  it("shows no banner before any flush", () => {
    render(<MemorySection />);
    expect(banner()).toBeNull();
  });

  it("surfaces the auto-curation banner after a flush, pluralized", () => {
    seed(1, 2);
    render(<MemorySection />);
    const b = banner();
    expect(b).not.toBeNull();
    expect(b?.textContent).toContain("auto-curated");
    expect(b?.textContent).toContain("2 new entries");
  });

  it("uses the singular for a single write", () => {
    seed(1, 1);
    render(<MemorySection />);
    expect(banner()?.textContent).toContain("1 new entry");
  });

  it("dismisses the banner until the next flush bumps the count", () => {
    seed(1, 2);
    render(<MemorySection />);
    expect(banner()).not.toBeNull();

    click(container.querySelector('[aria-label="Dismiss"]'));
    expect(banner()).toBeNull();

    // A later flush re-raises it.
    act(() => useMemoryStore.getState().noteFlush(3));
    expect(banner()).not.toBeNull();
    expect(banner()?.textContent).toContain("3 new entries");
  });
});
