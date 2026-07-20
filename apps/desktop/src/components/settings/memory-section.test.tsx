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
    savingStratum: null,
    saveError: null,
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

/** Seed a curated body so the category tabs render (one file keeps the
 *  `loading && files.length === 0` gate from hiding the content). */
function seedCurated(curatedBody: string) {
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
    curatedBody,
    loading: false,
    query: "",
    load: async () => {},
  });
}

const LONG_IDENTITY = Array.from(
  { length: 14 },
  (_, i) => `Identity line ${i + 1}`,
).join("\n");
const FIXTURE = `## Identity\n${LONG_IDENTITY}\n\n## Patterns\nShort pattern with keyword.\n\n## Focus\nShort focus text.\n`;

/** Radix Tabs uses automatic activation: focusing a trigger selects it (see
 *  the `SubTabs` tests in `primitives.test.tsx`) — a raw synthetic click
 *  doesn't move focus under jsdom, so activate by focusing. */
function activateTab(el: Element | null | undefined) {
  act(() => {
    (el as HTMLElement | null)?.focus();
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

describe("MemorySection — category tabs + reading view (#906)", () => {
  function tabs(): HTMLElement[] {
    return [...container.querySelectorAll<HTMLElement>('[role="tab"]')];
  }

  function activeTab(): HTMLElement | undefined {
    return tabs().find((t) => t.getAttribute("data-state") === "active");
  }

  beforeEach(() => {
    seedCurated(FIXTURE);
  });

  it("defaults to the Identity tab", () => {
    render(<MemorySection />);
    expect(activeTab()?.textContent).toContain("Identity");
    expect(container.textContent).toContain("Identity line 1");
  });

  it("switches the rendered body when another tab is activated", () => {
    render(<MemorySection />);
    activateTab(tabs().find((t) => t.textContent?.includes("Patterns")));
    expect(activeTab()?.textContent).toContain("Patterns");
    expect(container.textContent).toContain("Short pattern with keyword.");
    expect(container.textContent).not.toContain("Identity line 1");
  });

  it("shows See more only for a body longer than the clamp", () => {
    render(<MemorySection />);
    const seeMore = () =>
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent === "See more",
      );
    expect(seeMore()).toBeTruthy();

    activateTab(tabs().find((t) => t.textContent?.includes("Patterns")));
    expect(seeMore()).toBeUndefined();
  });

  it("opens the full reading view from See more, hiding the other sections", () => {
    render(<MemorySection />);
    click(
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent === "See more",
      ),
    );
    expect(container.textContent).toContain("Back");
    expect(container.textContent).toContain("Identity line 14");
    expect(container.textContent).not.toContain("Journal");
    expect(container.textContent).not.toContain("Files");
    expect(container.textContent).not.toContain("Salience");
  });

  it("returns from the reading view via Back", () => {
    render(<MemorySection />);
    click(
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent === "See more",
      ),
    );
    click(
      [...container.querySelectorAll("button")].find((b) =>
        b.textContent?.includes("Back"),
      ),
    );
    expect(container.textContent).not.toContain("Back");
    expect(container.textContent).toContain("Journal");
    expect(container.textContent).toContain("Files");
    expect(container.textContent).toContain("Salience");
  });

  // --- Curated-stratum editing via <MarkdownEditor> (#868) ---
  function openReadingView() {
    render(<MemorySection />);
    click(
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent === "See more",
      ),
    );
  }
  const btn = (text: string) =>
    [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes(text),
    );

  it("reading view shows an Edit affordance; entering edit mode swaps to Save/Cancel", () => {
    openReadingView();
    expect(btn("Edit")).toBeTruthy();
    expect(btn("Save")).toBeUndefined();

    click(btn("Edit"));
    expect(btn("Save")).toBeTruthy();
    expect(btn("Cancel")).toBeTruthy();
    // Edit affordance is gone while editing.
    expect(
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent?.trim() === "Edit",
      ),
    ).toBeUndefined();
  });

  it("Save starts disabled — a fresh draft equals the saved body", () => {
    openReadingView();
    click(btn("Edit"));
    // draft === body on entry, so there's nothing to persist yet. The
    // dirty→save round-trip (onChange → writeCuratedMemory → reload) is covered
    // by store/memory.test.ts; the editor's onChange wiring by
    // markdown-editor.test.tsx.
    expect((btn("Save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("Cancel exits edit mode without persisting", () => {
    const spy = vi
      .spyOn(useMemoryStore.getState(), "saveCuratedStratum")
      .mockResolvedValue(true);
    openReadingView();
    click(btn("Edit"));
    click(btn("Cancel"));
    expect(btn("Save")).toBeUndefined();
    expect(btn("Edit")).toBeTruthy();
    expect(spy).not.toHaveBeenCalled();
  });

  it("surfaces a save error inline in the reading view", () => {
    openReadingView();
    click(btn("Edit"));
    act(() => useMemoryStore.setState({ saveError: "disk full" }));
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "disk full",
    );
  });

  it("auto-selects and badges the tab matching a search query", () => {
    render(<MemorySection />);
    act(() => useMemoryStore.getState().setQuery("keyword"));
    expect(activeTab()?.textContent).toContain("Patterns");
    const patternsTab = tabs().find((t) => t.textContent?.includes("Patterns"));
    const identityTab = tabs().find((t) => t.textContent?.includes("Identity"));
    expect(patternsTab?.querySelector("[aria-hidden]")).not.toBeNull();
    expect(identityTab?.querySelector("[aria-hidden]")).toBeNull();
  });

  it("does not move off a tab that already matches the query", () => {
    render(<MemorySection />);
    activateTab(tabs().find((t) => t.textContent?.includes("Patterns")));
    act(() => useMemoryStore.getState().setQuery("keyword"));
    expect(activeTab()?.textContent).toContain("Patterns");
  });

  it("shows No match when the active tab's body doesn't match the query", () => {
    render(<MemorySection />);
    act(() => useMemoryStore.getState().setQuery("nope-not-found"));
    expect(container.textContent).toContain("No match");
    expect(tabs().some((t) => t.querySelector("[aria-hidden]"))).toBe(false);
  });
});
