import { describe, expect, it } from "vitest";

import type { MemoryFileInfo } from "@/bindings/MemoryFileInfo";
import {
  buildFiles,
  buildJournal,
  categoryMatches,
  filterFiles,
  filterJournal,
  firstMeaningfulLine,
  formatBytes,
  formatMemoryFooter,
  parseCategories,
} from "@/lib/memory-view";

const CURATED = `# Memory

## Identity
Abid is the frontend owner; prefers concise answers and dark mode.

## Patterns
One PR per issue. Verify under the mock before pushing.

## Focus
Shipping the Settings Memory section (SET.8).
`;

const FILES: MemoryFileInfo[] = [
  {
    name: "MEMORY.md",
    relPath: "MEMORY.md",
    kind: "curated",
    sizeBytes: 200,
    modifiedMs: 3,
  },
  {
    name: "2026-06-18.md",
    relPath: "daily/2026-06-18.md",
    kind: "daily",
    sizeBytes: 41,
    modifiedMs: 2,
  },
  {
    name: "2026-06-17.md",
    relPath: "daily/2026-06-17.md",
    kind: "daily",
    sizeBytes: 33,
    modifiedMs: 1,
  },
];

describe("memory-view — parseCategories", () => {
  it("extracts Identity / Patterns / Focus bodies, heading lines dropped", () => {
    const cats = parseCategories(CURATED);
    expect(cats.identity).toContain("frontend owner");
    expect(cats.identity).not.toContain("## Identity");
    expect(cats.patterns).toContain("One PR per issue");
    expect(cats.focus).toContain("Settings Memory section");
  });

  it("returns empty strings for missing headings and a null body", () => {
    expect(parseCategories(null)).toEqual({
      identity: "",
      patterns: "",
      focus: "",
    });
    const partial = parseCategories("## Identity\nonly identity here\n");
    expect(partial.identity).toBe("only identity here");
    expect(partial.patterns).toBe("");
    expect(partial.focus).toBe("");
  });

  it("matches headings case-insensitively and stops at the next section", () => {
    const cats = parseCategories("## identity\na\n## Patterns\nb\n");
    expect(cats.identity).toBe("a");
    expect(cats.patterns).toBe("b");
  });
});

describe("memory-view — previews & journal", () => {
  it("firstMeaningfulLine strips bullets and skips blank lines", () => {
    expect(firstMeaningfulLine("\n\n- did a thing\nmore")).toBe("did a thing");
    expect(firstMeaningfulLine("# Heading\nbody")).toBe("Heading");
    expect(firstMeaningfulLine("   \n  ")).toBe("");
  });

  it("buildJournal keeps only daily files, lifts the date, derives a preview", () => {
    const journal = buildJournal(FILES, {
      "daily/2026-06-18.md": "- Shipped the memory IPC contract.\n",
    });
    expect(journal.map((e) => e.relPath)).toEqual([
      "daily/2026-06-18.md",
      "daily/2026-06-17.md",
    ]);
    expect(journal[0].date).toBe("2026-06-18");
    expect(journal[0].preview).toBe("Shipped the memory IPC contract.");
    // No body read for the second daily file → empty preview, no throw.
    expect(journal[1].preview).toBe("");
  });
});

describe("memory-view — buildFiles & filters", () => {
  it("buildFiles maps every file to the presentation subset", () => {
    const refs = buildFiles(FILES);
    expect(refs).toHaveLength(3);
    expect(refs[0]).toEqual({
      name: "MEMORY.md",
      relPath: "MEMORY.md",
      kind: "curated",
      sizeBytes: 200,
    });
  });

  it("categoryMatches: empty query always matches, otherwise substring", () => {
    expect(categoryMatches("dark mode", "")).toBe(true);
    expect(categoryMatches("dark mode", "DARK")).toBe(true);
    expect(categoryMatches("dark mode", "light")).toBe(false);
  });

  it("filterJournal matches on date or preview", () => {
    const journal = buildJournal(FILES, {
      "daily/2026-06-18.md": "Shipped the contract\n",
      "daily/2026-06-17.md": "Reviewed the RFC\n",
    });
    expect(filterJournal(journal, "shipped")).toHaveLength(1);
    expect(filterJournal(journal, "2026-06-17")).toHaveLength(1);
    expect(filterJournal(journal, "")).toHaveLength(2);
    expect(filterJournal(journal, "nope")).toHaveLength(0);
  });

  it("filterFiles matches on file name", () => {
    const refs = buildFiles(FILES);
    expect(filterFiles(refs, "memory")).toHaveLength(1);
    expect(filterFiles(refs, "2026-06")).toHaveLength(2);
    expect(filterFiles(refs, "")).toHaveLength(3);
  });
});

describe("memory-view — formatting", () => {
  it("formatBytes humanizes with one decimal, trailing .0 dropped", () => {
    expect(formatBytes(64)).toBe("64 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(1024 * 1024 * 3)).toBe("3 MB");
  });

  it("formatMemoryFooter renders count + summed size, pluralized", () => {
    expect(formatMemoryFooter(3, 1536)).toBe("3 files · 1.5 KB");
    expect(formatMemoryFooter(1, 64)).toBe("1 file · 64 B");
  });
});
