// SET.8 Memory section (#131) — pure view-model helpers that map the frozen
// RFC 0006 memory IPC (`listMemoryFiles` / `readMemoryFile` / `memoryOverview`)
// onto the section's surfaces: three curated category cards, a JOURNAL list, and
// a FILES list with a footer.
//
// Section identity — the Identity / Patterns / Focus cards and the `## Identity`
// `## Patterns` `## Focus` headings they parse — is the RFC 0008 (Biosphere)
// soft-contract, so those labels are authoritative. The row *types* below stay
// FE-shaped (derived presentation, not a wire contract): the backend exposes
// whole-file Markdown, and this module is where we interpret it.

import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";
import type { MemoryFileInfo } from "@/bindings/MemoryFileInfo";
import type { MemoryFileKind } from "@/bindings/MemoryFileKind";

/** The three curated categories, parsed from `MEMORY.md` headings (RFC 0008 §3). */
export type MemoryCategoryId = "identity" | "patterns" | "focus";

/** Curated category bodies keyed by id; "" when the heading is absent. */
export type MemoryCategories = Record<MemoryCategoryId, string>;

/** One JOURNAL row, derived from a `daily/*.md` file. */
export interface MemoryJournalEntry {
  /** Stable key — the file's root-relative path. */
  relPath: string;
  /** Date string lifted from the file name (e.g. `2026-06-18`); "" if unparsable. */
  date: string;
  /** First meaningful line of the file body, list/heading markers stripped. */
  preview: string;
}

/** One FILES row — the presentation subset of `MemoryFileInfo`. */
export interface MemoryFileRef {
  name: string;
  relPath: string;
  kind: MemoryFileKind;
  sizeBytes: number;
}

/** Card label + subtitle for each category (RFC 0008 §3). Render order is fixed. */
export const MEMORY_CATEGORY_META: ReadonlyArray<{
  id: MemoryCategoryId;
  heading: string;
  label: string;
  subtitle: string;
}> = [
  {
    id: "identity",
    heading: "Identity",
    label: "Identity",
    subtitle: "Role, stable traits, hard preferences",
  },
  {
    id: "patterns",
    heading: "Patterns",
    label: "Patterns",
    subtitle: "Conventions, working style, recurring decisions",
  },
  {
    id: "focus",
    heading: "Focus",
    label: "Focus",
    subtitle: "Current priorities / active work",
  },
];

const EMPTY_CATEGORIES: MemoryCategories = {
  identity: "",
  patterns: "",
  focus: "",
};

/**
 * Parse the curated `MEMORY.md` body into the three categories. A section runs
 * from its `## <Heading>` line to the next `## ` heading (or end of file); the
 * heading line itself is dropped and the body is trimmed. Headings are matched
 * case-insensitively. Missing headings yield "".
 */
export function parseCategories(body: string | null): MemoryCategories {
  if (!body) return { ...EMPTY_CATEGORIES };
  const lines = body.split("\n");
  const result: MemoryCategories = { ...EMPTY_CATEGORIES };

  let current: MemoryCategoryId | null = null;
  const buffers: Record<MemoryCategoryId, string[]> = {
    identity: [],
    patterns: [],
    focus: [],
  };

  for (const line of lines) {
    const heading = /^##\s+(.+?)\s*$/.exec(line);
    if (heading) {
      const name = heading[1].toLowerCase();
      const match = MEMORY_CATEGORY_META.find(
        (m) => m.heading.toLowerCase() === name,
      );
      current = match ? match.id : null;
      continue;
    }
    if (current) buffers[current].push(line);
  }

  for (const id of Object.keys(buffers) as MemoryCategoryId[]) {
    result[id] = buffers[id].join("\n").trim();
  }
  return result;
}

/** Whether a category body should show under the current query (empty = always). */
export function categoryMatches(text: string, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === "") return true;
  return text.toLowerCase().includes(q);
}

/** Strip a leading list bullet / heading marker so previews read cleanly. */
function cleanPreviewLine(line: string): string {
  return line.replace(/^\s*(?:[-*+]\s+|#{1,6}\s+|>\s+)/, "").trim();
}

/** First non-empty, marker-stripped line of a body; "" when there is none. */
export function firstMeaningfulLine(body: string): string {
  for (const raw of body.split("\n")) {
    const cleaned = cleanPreviewLine(raw);
    if (cleaned !== "") return cleaned;
  }
  return "";
}

/** Lift a `YYYY-MM-DD` date from a daily file name, or "" if it has none. */
function dateFromName(name: string): string {
  const m = /(\d{4}-\d{2}-\d{2})/.exec(name);
  return m ? m[1] : "";
}

/**
 * Build the JOURNAL rows from the daily files, in the order the backend listed
 * them (curated-first / daily newest-first). `bodies` maps relPath → file body
 * for previews; a missing body just yields an empty preview.
 */
export function buildJournal(
  files: MemoryFileInfo[],
  bodies: Record<string, string>,
): MemoryJournalEntry[] {
  return files
    .filter((f) => f.kind === "daily")
    .map((f) => ({
      relPath: f.relPath,
      date: dateFromName(f.name),
      preview: bodies[f.relPath] ? firstMeaningfulLine(bodies[f.relPath]) : "",
    }));
}

/** Build the FILES rows from every memory file (all kinds), order preserved. */
export function buildFiles(files: MemoryFileInfo[]): MemoryFileRef[] {
  return files.map((f) => ({
    name: f.name,
    relPath: f.relPath,
    kind: f.kind,
    sizeBytes: f.sizeBytes,
  }));
}

/** Filter journal rows by a substring query over the date and preview. */
export function filterJournal(
  entries: MemoryJournalEntry[],
  query: string,
): MemoryJournalEntry[] {
  const q = query.trim().toLowerCase();
  if (q === "") return entries;
  return entries.filter(
    (e) =>
      e.date.toLowerCase().includes(q) || e.preview.toLowerCase().includes(q),
  );
}

/** Filter file rows by a substring query over the file name. */
export function filterFiles(
  refs: MemoryFileRef[],
  query: string,
): MemoryFileRef[] {
  const q = query.trim().toLowerCase();
  if (q === "") return refs;
  return refs.filter((f) => f.name.toLowerCase().includes(q));
}

/** Humanize a byte count: `64 B`, `1.5 KB`, `2 MB` (one decimal, trailing `.0` dropped). */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${round1(kb)} KB`;
  return `${round1(kb / 1024)} MB`;
}

function round1(n: number): string {
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}

/** The FILES footer, e.g. `3 files · 1.5 KB`. */
export function formatMemoryFooter(
  fileCount: number,
  totalBytes: number,
): string {
  const noun = fileCount === 1 ? "file" : "files";
  return `${fileCount} ${noun} · ${formatBytes(totalBytes)}`;
}

// ── Salience surface (M6.2, #293) ────────────────────────────────────────────
// Presentation helpers over the backend-authoritative `MemoryChunkStat`. The
// backend owns `weight` (effective, pin-aware) and `dormant`; these helpers only
// shape that for the list row — they never re-derive the dormancy threshold.

const ONE_DAY_MS = 86_400_000;

/** Effective weight (0–1) → an integer 0–100 percentage for the `<Progress>` bar. */
export function weightPercent(weight: number): number {
  return Math.round(Math.min(1, Math.max(0, weight)) * 100);
}

/**
 * Humanize a chunk's last-recalled time for the row's age line. The clock starts
 * at first *recall*, not creation, so a chunk with no `chunk_stats` row reads
 * `null` → "never recalled" (not an epoch timestamp). Buckets mirror the
 * backend's `human_age_ms` tagging (#373) for consistency with `memory_search`.
 */
export function humanAge(
  lastAccessedMs: number | null,
  now: number = Date.now(),
): string {
  if (lastAccessedMs === null) return "never recalled";
  const days = Math.floor((now - lastAccessedMs) / ONE_DAY_MS);
  if (days <= 0) return "today";
  if (days < 7) return `~${days} ${days === 1 ? "day" : "days"} ago`;
  if (days < 35) {
    const weeks = Math.round(days / 7);
    return `~${weeks} ${weeks === 1 ? "week" : "weeks"} ago`;
  }
  if (days < 365) {
    const months = Math.round(days / 30);
    return `~${months} ${months === 1 ? "month" : "months"} ago`;
  }
  const years = Math.round(days / 365);
  return `~${years} ${years === 1 ? "year" : "years"} ago`;
}

/**
 * Order chunks for the Salience list: pinned first (the user held them), then by
 * effective weight ascending so the coldest/dormant chunks — the ones a user
 * might want to wake or prune from attention — surface to the top. Stable, pure.
 */
export function sortChunks(chunks: MemoryChunkStat[]): MemoryChunkStat[] {
  return [...chunks].sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
    return a.weight - b.weight;
  });
}

/** Filter chunks by a substring query over the heading, preview, and relPath. */
export function filterChunks(
  chunks: MemoryChunkStat[],
  query: string,
): MemoryChunkStat[] {
  const q = query.trim().toLowerCase();
  if (q === "") return chunks;
  return chunks.filter(
    (c) =>
      (c.heading?.toLowerCase().includes(q) ?? false) ||
      c.preview.toLowerCase().includes(q) ||
      c.relPath.toLowerCase().includes(q),
  );
}
