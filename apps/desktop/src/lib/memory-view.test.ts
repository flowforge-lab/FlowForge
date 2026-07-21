import { describe, expect, it } from "vitest";

import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";
import type { MemoryFileInfo } from "@/bindings/MemoryFileInfo";
import {
  buildFiles,
  buildJournal,
  categoryMatches,
  clampCategoryBody,
  filterChunks,
  filterFiles,
  filterJournal,
  firstMeaningfulLine,
  formatBytes,
  formatMemoryFooter,
  humanAge,
  matchingCategoryIds,
  parseCategories,
  replaceCategory,
  sortChunks,
  stratumFromHeadingLine,
  weightPercent,
  type MemoryCategories,
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

  it("keeps a note's own `## sub-heading` in the body, not as a boundary (#774)", () => {
    // `memory_write` encourages `## Heading` inside a note. Such a heading is
    // not one of the three strata, so it must NOT reset the section — its line
    // and everything after it stays in the current card.
    const cats = parseCategories(
      "## Patterns\n## FlowForge tooling\n- use the diagnostics tool\n",
    );
    expect(cats.patterns).toBe(
      "## FlowForge tooling\n- use the diagnostics tool",
    );
    expect(cats.identity).toBe("");
    expect(cats.focus).toBe("");
  });

  it("retains content before and after a mid-body `## sub-heading`, and a following stratum still parses", () => {
    const cats = parseCategories(
      [
        "## Focus",
        "Shipping the memory panel.",
        "## Roadmap",
        "- editable strata next",
        "## Identity",
        "Abid, frontend owner",
        "",
      ].join("\n"),
    );
    // The whole Focus body — the prose, the sub-heading, and its bullet.
    expect(cats.focus).toBe(
      "Shipping the memory panel.\n## Roadmap\n- editable strata next",
    );
    // The real `## Identity` stratum after it is unaffected.
    expect(cats.identity).toBe("Abid, frontend owner");
    expect(cats.patterns).toBe("");
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

describe("memory-view — salience helpers (M6.2, #293)", () => {
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

  it("weightPercent maps 0–1 to 0–100 and clamps out-of-range", () => {
    expect(weightPercent(0)).toBe(0);
    expect(weightPercent(0.123)).toBe(12);
    expect(weightPercent(1)).toBe(100);
    expect(weightPercent(1.5)).toBe(100);
    expect(weightPercent(-0.2)).toBe(0);
  });

  it("humanAge renders 'never recalled' for a null timestamp", () => {
    expect(humanAge(null)).toBe("never recalled");
  });

  it("humanAge buckets by elapsed time relative to now", () => {
    const now = 1_000 * 86_400_000; // day 1000 in epoch-ms
    const daysAgo = (d: number) => now - d * 86_400_000;
    expect(humanAge(daysAgo(0), now)).toBe("today");
    expect(humanAge(daysAgo(1), now)).toBe("~1 day ago");
    expect(humanAge(daysAgo(3), now)).toBe("~3 days ago");
    expect(humanAge(daysAgo(14), now)).toBe("~2 weeks ago");
    expect(humanAge(daysAgo(180), now)).toBe("~6 months ago");
    expect(humanAge(daysAgo(400), now)).toBe("~1 year ago");
  });

  it("sortChunks puts pinned first, then ascending weight (dormant surfaced)", () => {
    const cold = chunk({ chunkKey: "cold", weight: 0.1, dormant: true });
    const warm = chunk({ chunkKey: "warm", weight: 0.8 });
    const pinned = chunk({ chunkKey: "pin", weight: 1, pinned: true });
    const sorted = sortChunks([warm, pinned, cold]);
    expect(sorted.map((c) => c.chunkKey)).toEqual(["pin", "cold", "warm"]);
  });

  it("sortChunks is pure — it does not mutate its input", () => {
    const input = [
      chunk({ chunkKey: "a", weight: 0.5 }),
      chunk({ chunkKey: "b", weight: 0.1 }),
    ];
    const snapshot = input.map((c) => c.chunkKey);
    sortChunks(input);
    expect(input.map((c) => c.chunkKey)).toEqual(snapshot);
  });

  it("filterChunks matches heading, preview, or relPath", () => {
    const chunks = [
      chunk({ chunkKey: "a", heading: "Identity", preview: "role" }),
      chunk({
        chunkKey: "b",
        relPath: "daily/2026-06-18.md",
        preview: "shipped",
      }),
    ];
    expect(filterChunks(chunks, "identity").map((c) => c.chunkKey)).toEqual([
      "a",
    ]);
    expect(filterChunks(chunks, "shipped").map((c) => c.chunkKey)).toEqual([
      "b",
    ]);
    expect(filterChunks(chunks, "2026-06").map((c) => c.chunkKey)).toEqual([
      "b",
    ]);
    expect(filterChunks(chunks, "")).toHaveLength(2);
    expect(filterChunks(chunks, "nope")).toHaveLength(0);
  });
});

describe("clampCategoryBody", () => {
  it("returns the body untouched when at or under the line limit", () => {
    const body = Array.from({ length: 12 }, (_, i) => `line ${i}`).join("\n");
    expect(clampCategoryBody(body)).toEqual({
      preview: body,
      truncated: false,
    });
  });

  it("truncates to maxLines and flags it", () => {
    const lines = Array.from({ length: 20 }, (_, i) => `line ${i}`);
    const { preview, truncated } = clampCategoryBody(lines.join("\n"));
    expect(truncated).toBe(true);
    expect(preview).toBe(lines.slice(0, 12).join("\n"));
  });

  it("treats an empty body as not truncated", () => {
    expect(clampCategoryBody("")).toEqual({ preview: "", truncated: false });
  });

  it("respects a custom maxLines", () => {
    const lines = Array.from({ length: 5 }, (_, i) => `line ${i}`);
    const { preview, truncated } = clampCategoryBody(lines.join("\n"), 3);
    expect(truncated).toBe(true);
    expect(preview).toBe(lines.slice(0, 3).join("\n"));
  });
});

describe("matchingCategoryIds", () => {
  const categories: MemoryCategories = {
    identity: "alpha",
    patterns: "beta keyword",
    focus: "",
  };

  it("returns all ids for an empty query", () => {
    expect(matchingCategoryIds(categories, "")).toEqual([
      "identity",
      "patterns",
      "focus",
    ]);
  });

  it("filters to categories whose body contains the query", () => {
    expect(matchingCategoryIds(categories, "keyword")).toEqual(["patterns"]);
  });

  it("excludes an empty category from a non-empty query", () => {
    expect(matchingCategoryIds(categories, "anything")).not.toContain("focus");
  });

  it("returns an empty array when nothing matches", () => {
    expect(matchingCategoryIds(categories, "nope")).toEqual([]);
  });
});

// #868 — the FE mirror of the backend's `replace_under_heading`. These cases
// track the Rust tests in crates/ff-memory/src/tests.rs so the mock and the real
// write seam agree.
describe("replaceCategory", () => {
  const file = `# Memory

## Identity
old identity

## Patterns
old patterns

## Focus
old focus
`;

  it("overwrites a section body instead of appending to it", () => {
    const out = replaceCategory(file, "identity", "new identity");
    expect(parseCategories(out).identity).toBe("new identity");
    expect(out).not.toContain("old identity");
  });

  it("preserves sibling sections and the file title", () => {
    const out = replaceCategory(file, "patterns", "new patterns");
    const parsed = parseCategories(out);
    expect(parsed.identity).toBe("old identity");
    expect(parsed.focus).toBe("old focus");
    expect(out.startsWith("# Memory")).toBe(true);
  });

  it("replaces a trailing (EOF) section", () => {
    const out = replaceCategory(file, "focus", "new focus");
    expect(parseCategories(out).focus).toBe("new focus");
    expect(parseCategories(out).patterns).toBe("old patterns");
  });

  it("creates a missing heading at the end of the file", () => {
    const out = replaceCategory("# Memory\n", "focus", "shipping #868");
    expect(out).toContain("## Focus");
    expect(parseCategories(out).focus).toBe("shipping #868");
  });

  it("clears the body but keeps the heading for empty text", () => {
    const out = replaceCategory(file, "identity", "");
    expect(out).toContain("## Identity");
    expect(parseCategories(out).identity).toBe("");
    expect(parseCategories(out).patterns).toBe("old patterns");
  });

  it("round-trips an unchanged body through parse → replace", () => {
    const parsed = parseCategories(file);
    const out = replaceCategory(file, "patterns", parsed.patterns);
    expect(parseCategories(out)).toEqual(parsed);
  });

  it("handles a null body (no curated file yet)", () => {
    const out = replaceCategory(null, "identity", "first fact");
    expect(out).toBe("## Identity\nfirst fact\n");
  });
});

// Read and write share one section boundary (`stratumFromHeadingLine`), so a
// stratum carrying a note's own `## sub-heading` (#774) survives an edit. This
// used to be the #868 data-loss path: read kept the sub-heading in the body,
// write truncated at it and re-appended the tail as a sibling section.
describe("replaceCategory — round-trip conservation (#868/#774)", () => {
  const file = "## Identity\nfacts\n## Notes\ndetail\n\n## Focus\nnow\n";

  it("is a no-op when an unchanged body containing a `## sub-heading` is saved", () => {
    const parsed = parseCategories(file);
    expect(parsed.identity).toBe("facts\n## Notes\ndetail");

    const out = replaceCategory(file, "identity", parsed.identity);
    expect(parseCategories(out)).toEqual(parsed);
    expect(out.match(/## Notes/g)).toHaveLength(1);
  });

  it("conserves every stratum when each is saved back unchanged in turn", () => {
    const parsed = parseCategories(file);
    let out = file;
    for (const id of ["identity", "patterns", "focus"] as const) {
      out = replaceCategory(out, id, parsed[id]);
    }
    expect(parseCategories(out)).toEqual(parsed);
  });

  it("replaces the whole body, sub-heading included, on a real edit", () => {
    const out = replaceCategory(file, "identity", "new identity");
    expect(parseCategories(out).identity).toBe("new identity");
    expect(out).not.toContain("## Notes");
    // The sibling stratum is untouched.
    expect(parseCategories(out).focus).toBe("now");
  });

  it("rewrites a lowercase heading in place rather than duplicating it", () => {
    const out = replaceCategory("## identity\nold\n", "identity", "new");
    expect(parseCategories(out).identity).toBe("new");
    expect(out.toLowerCase().match(/## identity/g)).toHaveLength(1);
  });
});

describe("stratumFromHeadingLine — the one shared boundary", () => {
  it("matches the three strata case-insensitively", () => {
    expect(stratumFromHeadingLine("## Identity")).toBe("identity");
    expect(stratumFromHeadingLine("## focus  ")).toBe("focus");
  });

  it("treats a note's own sub-heading and non-headings as body", () => {
    expect(stratumFromHeadingLine("## Roadmap")).toBeNull();
    expect(stratumFromHeadingLine("### Identity")).toBeNull();
    expect(stratumFromHeadingLine("##Identity")).toBeNull();
    expect(stratumFromHeadingLine("Identity")).toBeNull();
  });
});
