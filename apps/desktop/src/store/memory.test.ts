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
    chunks: [],
    chunkBusy: {},
    savingStratum: null,
    saveError: null,
    query: "",
    loading: false,
    error: null,
    flushCount: 0,
    lastFlushWrites: 0,
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

  it("noteFlush records flush provenance (#283)", () => {
    expect(useMemoryStore.getState().flushCount).toBe(0);

    useMemoryStore.getState().noteFlush(2);
    expect(useMemoryStore.getState().flushCount).toBe(1);
    expect(useMemoryStore.getState().lastFlushWrites).toBe(2);

    useMemoryStore.getState().noteFlush(1);
    expect(useMemoryStore.getState().flushCount).toBe(2);
    expect(useMemoryStore.getState().lastFlushWrites).toBe(1);
  });

  it("surfaces a load error from the IPC layer", async () => {
    vi.spyOn(ipc, "listMemoryFiles").mockRejectedValue(new Error("offline"));
    await useMemoryStore.getState().load();
    const s = useMemoryStore.getState();
    expect(s.error).toBe("offline");
    expect(s.loading).toBe(false);
  });

  it("load() populates per-chunk salience stats (M6.2, #293)", async () => {
    await useMemoryStore.getState().load();
    const { chunks } = useMemoryStore.getState();
    expect(chunks.length).toBeGreaterThan(0);
    // The mock seeds one dormant and one pinned chunk.
    expect(chunks.some((c) => c.dormant)).toBe(true);
    expect(chunks.some((c) => c.pinned)).toBe(true);
  });

  it("resetChunk wakes a dormant chunk back to full weight", async () => {
    await useMemoryStore.getState().load();
    const dormant = useMemoryStore
      .getState()
      .chunks.find((c) => c.dormant && !c.pinned);
    expect(dormant).toBeTruthy();

    await useMemoryStore.getState().resetChunk(dormant!.chunkKey);

    const after = useMemoryStore
      .getState()
      .chunks.find((c) => c.chunkKey === dormant!.chunkKey);
    expect(after?.weight).toBe(1);
    expect(after?.dormant).toBe(false);
    expect(after?.lastAccessedMs).not.toBeNull();
    // Busy flag is cleared once the mutation settles.
    expect(
      useMemoryStore.getState().chunkBusy[dormant!.chunkKey],
    ).toBeUndefined();
  });

  it("setPinned round-trips and a pinned chunk is never dormant", async () => {
    await useMemoryStore.getState().load();
    // Pick any unpinned chunk — the mock is a shared singleton, so an earlier
    // test may have woken the dormant one; we only need pin to round-trip here.
    const target = useMemoryStore.getState().chunks.find((c) => !c.pinned);
    expect(target).toBeTruthy();

    await useMemoryStore.getState().setPinned(target!.chunkKey, true);
    let row = useMemoryStore
      .getState()
      .chunks.find((c) => c.chunkKey === target!.chunkKey);
    expect(row?.pinned).toBe(true);
    expect(row?.dormant).toBe(false);

    await useMemoryStore.getState().setPinned(target!.chunkKey, false);
    row = useMemoryStore
      .getState()
      .chunks.find((c) => c.chunkKey === target!.chunkKey);
    expect(row?.pinned).toBe(false);
  });

  it("surfaces a mutation error and clears the busy flag", async () => {
    await useMemoryStore.getState().load();
    const key = useMemoryStore.getState().chunks[0].chunkKey;
    vi.spyOn(ipc, "resetMemoryChunk").mockRejectedValue(
      new Error("write failed"),
    );

    await useMemoryStore.getState().resetChunk(key);
    const s = useMemoryStore.getState();
    expect(s.error).toBe("write failed");
    expect(s.chunkBusy[key]).toBeUndefined();
  });

  it("guards against re-entrant mutations on a busy chunk", async () => {
    await useMemoryStore.getState().load();
    const key = useMemoryStore.getState().chunks[0].chunkKey;
    const spy = vi.spyOn(ipc, "setMemoryChunkPinned");

    // Mark the row busy, then a second call should no-op (not hit the IPC).
    useMemoryStore.setState({ chunkBusy: { [key]: true } });
    await useMemoryStore.getState().setPinned(key, true);
    expect(spy).not.toHaveBeenCalled();
  });

  it("saveCuratedStratum writes via IPC then reloads with the new body (#868)", async () => {
    await useMemoryStore.getState().load();
    const spy = vi.spyOn(ipc, "writeCuratedMemory");

    const ok = await useMemoryStore
      .getState()
      .saveCuratedStratum("focus", "Brand new focus body");

    expect(ok).toBe(true);
    expect(spy).toHaveBeenCalledWith("focus", "Brand new focus body");
    const s = useMemoryStore.getState();
    // Reloaded body reflects the write (mock mutates the fixture MEMORY.md).
    expect(parseCategories(s.curatedBody).focus).toContain(
      "Brand new focus body",
    );
    // Other strata are untouched by a whole-stratum replace.
    expect(parseCategories(s.curatedBody).identity).not.toBe("");
    expect(s.savingStratum).toBeNull();
    expect(s.saveError).toBeNull();
  });

  it("saveCuratedStratum surfaces a write error and does not reload", async () => {
    await useMemoryStore.getState().load();
    const before = useMemoryStore.getState().curatedBody;
    vi.spyOn(ipc, "writeCuratedMemory").mockRejectedValue(
      new Error("disk full"),
    );

    const ok = await useMemoryStore
      .getState()
      .saveCuratedStratum("identity", "nope");

    expect(ok).toBe(false);
    const s = useMemoryStore.getState();
    expect(s.saveError).toBe("disk full");
    expect(s.savingStratum).toBeNull();
    // Body unchanged — a failed write must not mutate the pane's view.
    expect(s.curatedBody).toBe(before);
  });
});
