// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type MockedFunction,
} from "vitest";
import { MemorySection } from "@/components/settings/memory-section";
import { useMemoryStore, type MemoryState } from "@/store/memory";
import { useSettingsStore } from "@/store/settings";
import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";

// Milkdown builds its ProseMirror view asynchronously and needs a real layout,
// so stand in a plain textarea with the same value/onChange contract. The real
// editor is covered by `components/ui/markdown-editor.test.tsx`; what matters
// here is the surrounding Edit → Save/Cancel wiring.
vi.mock("@/components/ui/markdown-editor", () => ({
  MarkdownEditor: ({
    value,
    onChange,
  }: {
    value: string;
    onChange: (md: string) => void;
  }) => (
    <textarea
      data-testid="md-editor"
      defaultValue={value}
      onChange={(e) => onChange(e.target.value)}
    />
  ),
}));

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
    writeBusy: false,
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

describe("MemorySection — editable curated strata (#868)", () => {
  function buttonNamed(name: string): HTMLButtonElement | undefined {
    return [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === name,
    );
  }

  function editor(): HTMLTextAreaElement | null {
    return container.querySelector<HTMLTextAreaElement>(
      '[data-testid="md-editor"]',
    );
  }

  /** React tracks the DOM value internally, so a bare `el.value = …` is
   *  swallowed as a no-op change — go through the native setter first. */
  function type(text: string) {
    const el = editor()!;
    act(() => {
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )?.set?.call(el, text);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  /** The discard confirm renders in a Radix portal, outside `container`. */
  function dialog(): HTMLElement | null {
    return document.body.querySelector('[role="alertdialog"]');
  }

  let writeStratum: MockedFunction<MemoryState["writeStratum"]>;

  beforeEach(() => {
    seedCurated(FIXTURE);
    writeStratum = vi.fn(async () => true);
    useMemoryStore.setState({ writeStratum });
  });

  it("offers Edit on every stratum tab, including a short un-truncated one", () => {
    render(<MemorySection />);
    // Identity is long enough to truncate; Focus is not — both still get Edit.
    expect(buttonNamed("Edit")).toBeTruthy();

    const focusTab = [
      ...container.querySelectorAll<HTMLElement>('[role="tab"]'),
    ].find((t) => t.textContent?.includes("Focus"));
    act(() => focusTab?.focus());
    expect(container.textContent).not.toContain("See more");
    expect(buttonNamed("Edit")).toBeTruthy();
  });

  it("Edit opens the reading view already in edit mode, seeded with the body", () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));

    expect(container.textContent).toContain("Back");
    expect(editor()).not.toBeNull();
    expect(editor()?.value).toContain("Identity line 14");
    // The other panel surfaces are replaced, not stacked (no nested scroll).
    expect(container.textContent).not.toContain("Journal");
  });

  it("Save is disabled until the body actually changes", () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    expect(buttonNamed("Save")?.disabled).toBe(true);

    type("edited identity");
    expect(buttonNamed("Save")?.disabled).toBe(false);
  });

  it("Save writes the edited body for the open stratum and leaves edit mode", async () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("edited identity");

    await act(async () => {
      buttonNamed("Save")?.dispatchEvent(
        new MouseEvent("click", { bubbles: true }),
      );
    });

    expect(writeStratum).toHaveBeenCalledWith("identity", "edited identity");
    expect(editor()).toBeNull();
  });

  it("keeps the buffer and stays in edit mode when the write fails", async () => {
    writeStratum.mockResolvedValue(false);
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("edited identity");

    await act(async () => {
      buttonNamed("Save")?.dispatchEvent(
        new MouseEvent("click", { bubbles: true }),
      );
    });

    expect(editor()).not.toBeNull();
    expect(buttonNamed("Save")?.disabled).toBe(false);
  });

  it("Cancel with no changes exits straight to the reading view", () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    click(buttonNamed("Cancel"));

    expect(dialog()).toBeNull();
    expect(editor()).toBeNull();
    expect(container.textContent).toContain("Identity line 14");
  });

  it("Cancel with unsaved changes asks before discarding", () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("edited identity");
    click(buttonNamed("Cancel"));

    expect(dialog()).not.toBeNull();
    expect(dialog()?.textContent).toContain("Discard unsaved changes?");
    // Still editing until the user confirms.
    expect(editor()).not.toBeNull();

    click(
      [...document.body.querySelectorAll("button")].find(
        (b) => b.textContent?.trim() === "Discard",
      ),
    );
    expect(editor()).toBeNull();
    expect(writeStratum).not.toHaveBeenCalled();
  });

  it("asks before a Save that would clear the section", async () => {
    // An empty save clears the stratum (the backend keeps only the heading), so
    // select-all + delete + Save must not wipe it silently.
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("   ");

    click(buttonNamed("Save"));
    expect(dialog()?.textContent).toContain("Clear the Identity section?");
    expect(writeStratum).not.toHaveBeenCalled();

    await act(async () => {
      [...document.body.querySelectorAll("button")]
        .find((b) => b.textContent?.trim() === "Clear section")
        ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(writeStratum).toHaveBeenCalledWith("identity", "   ");
  });

  it("saves a non-empty body without any confirmation", async () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("still has content");

    await act(async () => {
      buttonNamed("Save")?.dispatchEvent(
        new MouseEvent("click", { bubbles: true }),
      );
    });
    expect(dialog()).toBeNull();
    expect(writeStratum).toHaveBeenCalledWith("identity", "still has content");
  });

  it("does not ask when clearing a stratum that is already empty", async () => {
    seedCurated("## Identity\n\n## Patterns\nSomething.\n");
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("  ");

    // Nothing to lose — Save stays disabled because the draft isn't dirty.
    expect(buttonNamed("Save")?.disabled).toBe(true);
    expect(dialog()).toBeNull();
  });

  it("Back with unsaved changes asks before leaving the reading view", () => {
    render(<MemorySection />);
    click(buttonNamed("Edit"));
    type("edited identity");
    click(
      [...container.querySelectorAll("button")].find((b) =>
        b.textContent?.includes("Back"),
      ),
    );

    expect(dialog()).not.toBeNull();
    expect(container.textContent).not.toContain("Journal");
  });

  it("leaves the journal and files surfaces read-only", () => {
    render(<MemorySection />);
    // The one Edit control belongs to the category pane; nothing below it.
    expect(
      [...container.querySelectorAll("button")].filter(
        (b) => b.textContent?.trim() === "Edit",
      ),
    ).toHaveLength(1);
  });
});
