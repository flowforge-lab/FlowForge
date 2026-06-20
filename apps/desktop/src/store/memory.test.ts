import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useMemoryStore } from "@/store/memory";
import {
  buildFiles,
  buildJournal,
  filterFiles,
  parseCategories,
} from "@/lib/memory-view";

function reset() {
  useMemoryStore.setState({
    files: [],
    overview: null,
    curatedBody: null,
    journalBodies: {},
    query: "",
    loading: false,
    error: null,
  });
}

describe("memory store (SET.8, #131)", () => {
  beforeEach(reset);
  afterEach(() => vi.restoreAllMocks());

  it("load() populates files, overview, curated body, and daily previews", async () => {
    await useMemoryStore.getState().load();
    const s = useMemoryStore.getState();

    expect(s.loading).toBe(false);
    expect(s.error).toBeNull();
    expect(s.files.length).toBeGreaterThan(0);
    expect(s.overview?.fileCount).toBe(s.files.length);
    // Curated body carries the canonical headings (parsed into cards).
    expect(s.curatedBody).toContain("## Identity");
    // Daily bodies are read for previews.
    const daily = s.files.find((f) => f.kind === "daily");
    expect(daily && s.journalBodies[daily.relPath]).toBeTruthy();
  });

  it("derives the three category cards from the curated body", async () => {
    await useMemoryStore.getState().load();
    const cats = parseCategories(useMemoryStore.getState().curatedBody);
    expect(cats.identity).not.toBe("");
    expect(cats.patterns).not.toBe("");
    expect(cats.focus).not.toBe("");
  });

  it("setQuery filters the derived files view; resetSearch clears it", async () => {
    await useMemoryStore.getState().load();
    useMemoryStore.getState().setQuery("MEMORY");

    const s = useMemoryStore.getState();
    const visible = filterFiles(buildFiles(s.files), s.query);
    expect(visible.length).toBe(1);
    expect(visible[0].name).toBe("MEMORY.md");

    useMemoryStore.getState().resetSearch();
    expect(useMemoryStore.getState().query).toBe("");
    expect(
      filterFiles(buildFiles(s.files), useMemoryStore.getState().query).length,
    ).toBe(s.files.length);
  });

  it("builds journal rows from daily files only", async () => {
    await useMemoryStore.getState().load();
    const s = useMemoryStore.getState();
    const journal = buildJournal(s.files, s.journalBodies);
    expect(journal.length).toBe(
      s.files.filter((f) => f.kind === "daily").length,
    );
    expect(journal.every((e) => e.date !== "")).toBe(true);
  });

  it("surfaces a load error from the IPC layer", async () => {
    vi.spyOn(ipc, "listMemoryFiles").mockRejectedValue(new Error("offline"));
    await useMemoryStore.getState().load();
    const s = useMemoryStore.getState();
    expect(s.error).toBe("offline");
    expect(s.loading).toBe(false);
  });
});
