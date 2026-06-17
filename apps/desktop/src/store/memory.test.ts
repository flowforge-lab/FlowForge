import { beforeEach, describe, expect, it } from "vitest";

import {
  filterMemory,
  formatMemoryFileSize,
  memoryFilesFooter,
  memorySnapshotIsEmpty,
  type MemorySnapshot,
} from "@/lib/memory";
import { useMemoryStore } from "@/store/memory";

const FIXTURE: MemorySnapshot = {
  categories: {
    who: {
      subtitle: "Role & preferences",
      items: ["Frontend engineer", "TypeScript fan"],
    },
    how: {
      subtitle: "Patterns & conventions",
      items: ["Use Zustand", "Match existing style"],
    },
    what: {
      subtitle: "Current priorities",
      items: ["Ship SET.8", "RFC 0002"],
    },
  },
  journal: [
    { id: "1", date: "2026-06-15", content: "Memory browser kickoff" },
    { id: "2", date: "2026-06-14", content: "Profiles section review" },
  ],
  files: [
    { name: "user_instructions.md", sizeBytes: 2048 },
    { name: "memory/who.md", sizeBytes: 512 },
  ],
};

describe("filterMemory", () => {
  it("returns the full snapshot for an empty query", () => {
    const out = filterMemory(FIXTURE, "");
    expect(out.journal).toHaveLength(2);
    expect(out.files).toHaveLength(2);
    expect(out.categories.who.items).toHaveLength(2);
  });

  it("filters journal entries and files by substring", () => {
    const out = filterMemory(FIXTURE, "Profiles");
    expect(out.journal.map((e) => e.id)).toEqual(["2"]);
    expect(out.files).toEqual([]);
    expect(memorySnapshotIsEmpty(out)).toBe(false);
  });

  it("filters category items and keeps a subtitle-only hit's full item list", () => {
    const out = filterMemory(FIXTURE, "Role");
    expect(out.categories.who.items).toEqual(FIXTURE.categories.who.items);
    expect(out.categories.how.items).toEqual([]);
    expect(out.categories.what.items).toEqual([]);
  });

  it("reports empty when nothing matches", () => {
    const out = filterMemory(FIXTURE, "nomatch-xyz");
    expect(memorySnapshotIsEmpty(out)).toBe(true);
  });
});

describe("memoryFilesFooter", () => {
  it("sums count and bytes for the footer", () => {
    expect(memoryFilesFooter(FIXTURE.files)).toEqual({
      count: 2,
      totalBytes: 2560,
    });
  });
});

describe("formatMemoryFileSize", () => {
  it("formats sub-kilobyte and kilobyte sizes", () => {
    expect(formatMemoryFileSize(512)).toBe("512 B");
    expect(formatMemoryFileSize(2048)).toBe("2 KB");
  });
});

describe("useMemoryStore", () => {
  beforeEach(() => {
    useMemoryStore.setState({
      snapshot: null,
      query: "",
      loading: false,
      error: null,
    });
  });

  it("loads the memory snapshot from IPC", async () => {
    await useMemoryStore.getState().load();
    const { snapshot, loading, error } = useMemoryStore.getState();
    expect(loading).toBe(false);
    expect(error).toBeNull();
    expect(snapshot?.categories.who.subtitle).toMatch(/Role/i);
    expect(snapshot?.journal.length).toBeGreaterThan(0);
    expect(snapshot?.files.length).toBeGreaterThan(0);
  });

  it("setQuery updates the search string", () => {
    useMemoryStore.getState().setQuery("typescript");
    expect(useMemoryStore.getState().query).toBe("typescript");
  });

  it("resetMemory clears the search query", async () => {
    useMemoryStore.getState().setQuery("typescript");
    useMemoryStore.getState().resetMemory();
    expect(useMemoryStore.getState().query).toBe("");
  });
});
