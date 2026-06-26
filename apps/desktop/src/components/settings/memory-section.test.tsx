// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemorySection } from "@/components/settings/memory-section";
import { useMemoryStore } from "@/store/memory";
import { useSettingsStore } from "@/store/settings";
import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";

// Minimal shims so radix primitives (Switch/Tooltip) mount under jsdom.
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver ||= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
for (const m of [
  "hasPointerCapture",
  "setPointerCapture",
  "releasePointerCapture",
] as const) {
  // @ts-expect-error — patching jsdom prototypes
  Element.prototype[m] ||= () =>
    m === "hasPointerCapture" ? false : undefined;
}

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
    chunks: [],
    chunkBusy: {},
    query: "",
    loading: false,
    error: null,
    flushCount,
    lastFlushWrites,
  });
}

const chunk = (over: Partial<MemoryChunkStat>): MemoryChunkStat => ({
  chunkKey: "k",
  relPath: "MEMORY.md",
  heading: null,
  preview: "",
  weight: 1,
  accessCount: 0,
  lastAccessedMs: null,
  dormant: false,
  pinned: false,
  ...over,
});

/** Seed loaded state with chunks so the Salience section renders (one file keeps
 *  the `loading && files.length === 0` gate from hiding the content). */
function seedChunks(chunks: MemoryChunkStat[]) {
  useMemoryStore.setState({
    files: [
      {
        name: "MEMORY.md",
        relPath: "MEMORY.md",
        kind: "curated",
        sizeBytes: 1,
        modifiedMs: 1,
      },
    ],
    chunks,
    chunkBusy: {},
    loading: false,
    // No-op the mount load so it doesn't clobber the seeded chunks async.
    load: async () => {},
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

describe("MemorySection — Salience surface (M6.2, #293)", () => {
  it("renders a dormant badge for a dormant chunk", () => {
    seedChunks([
      chunk({
        chunkKey: "cold",
        heading: "Focus",
        weight: 0.12,
        dormant: true,
      }),
    ]);
    render(<MemorySection />);
    // The dormant badge is wrapped in a Tooltip trigger (which overrides the
    // badge's data-slot), so match on its text + amber tone instead.
    const dormantBadge = [...container.querySelectorAll("span")].find(
      (s) => s.textContent === "dormant",
    );
    expect(dormantBadge).toBeTruthy();
    expect(dormantBadge?.className).toContain("amber");
  });

  it("fires the pin toggle through the store", () => {
    const spy = vi
      .spyOn(useMemoryStore.getState(), "setPinned")
      .mockResolvedValue();
    seedChunks([chunk({ chunkKey: "warm", heading: "Identity", weight: 0.9 })]);
    render(<MemorySection />);

    click(container.querySelector('[role="switch"]'));
    expect(spy).toHaveBeenCalledWith("warm", true);
  });

  it("disables the wake control while the row is busy", () => {
    seedChunks([
      chunk({
        chunkKey: "cold",
        heading: "Focus",
        weight: 0.12,
        dormant: true,
      }),
    ]);
    useMemoryStore.setState({ chunkBusy: { cold: true } });
    render(<MemorySection />);

    const wake = [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("Wake"),
    );
    expect(wake).toBeTruthy();
    expect((wake as HTMLButtonElement).disabled).toBe(true);
  });

  it("disables the wake control for a full-weight chunk (nothing to wake)", () => {
    seedChunks([chunk({ chunkKey: "full", heading: "Patterns", weight: 1 })]);
    render(<MemorySection />);
    const wake = [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("Wake"),
    );
    // Rendered but disabled (weight >= 1) — the user can't wake a full chunk.
    expect((wake as HTMLButtonElement | undefined)?.disabled).toBe(true);
  });

  it("shows the empty state when there are no chunks", () => {
    seedChunks([]);
    render(<MemorySection />);
    expect(container.textContent).toContain("No memory chunks yet");
  });
});
