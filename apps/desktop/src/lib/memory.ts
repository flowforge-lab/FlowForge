// PROVISIONAL memory types for the Settings → Memory section (SET.8, RFC 0002).
// There is no `ff-memory` crate or ts-rs binding yet — this shape is a reference-IA
// hypothesis for mock IPC only. Expect revision when the real memory model lands;
// `bindings/` stays untouched.

/** A single journal row — provisional. */
export interface MemoryJournalEntry {
  id: string;
  /** Display date (provisional — backend may use timestamps). */
  date: string;
  content: string;
}

/** A referenced memory file — provisional. */
export interface MemoryFileRef {
  name: string;
  sizeBytes: number;
}

/** One WHO / HOW / WHAT card body — provisional. */
export interface MemoryCategoryContent {
  /** e.g. "Role & preferences" — provisional subtitle copy. */
  subtitle: string;
  items: string[];
}

/** WHO / HOW / WHAT category payloads — provisional. */
export interface MemoryCategories {
  who: MemoryCategoryContent;
  how: MemoryCategoryContent;
  what: MemoryCategoryContent;
}

/** Full memory browser snapshot — provisional. */
export interface MemorySnapshot {
  categories: MemoryCategories;
  journal: MemoryJournalEntry[];
  files: MemoryFileRef[];
}

const matches = (haystack: string, needle: string) =>
  haystack.toLowerCase().includes(needle);

/** Client-side filter over a loaded snapshot (case-insensitive substring). */
export function filterMemory(
  snapshot: MemorySnapshot,
  rawQuery: string,
): MemorySnapshot {
  const q = rawQuery.trim().toLowerCase();
  if (q === "") return snapshot;

  const filterItems = (items: string[]) =>
    items.filter((item) => matches(item, q));

  const filterCategory = (
    cat: MemoryCategoryContent,
  ): MemoryCategoryContent | null => {
    const subtitleHit = matches(cat.subtitle, q);
    const items = filterItems(cat.items);
    if (!subtitleHit && items.length === 0) return null;
    return { ...cat, items: subtitleHit ? cat.items : items };
  };

  const who = filterCategory(snapshot.categories.who);
  const how = filterCategory(snapshot.categories.how);
  const what = filterCategory(snapshot.categories.what);

  return {
    categories: {
      who: who ?? { ...snapshot.categories.who, items: [] },
      how: how ?? { ...snapshot.categories.how, items: [] },
      what: what ?? { ...snapshot.categories.what, items: [] },
    },
    journal: snapshot.journal.filter(
      (e) => matches(e.content, q) || matches(e.date, q),
    ),
    files: snapshot.files.filter((f) => matches(f.name, q)),
  };
}

/** True when a filtered snapshot has nothing visible in any pane. */
export function memorySnapshotIsEmpty(snapshot: MemorySnapshot): boolean {
  const { categories, journal, files } = snapshot;
  const cats = [categories.who, categories.how, categories.what];
  return (
    cats.every((c) => c.items.length === 0) &&
    journal.length === 0 &&
    files.length === 0
  );
}

/** Sum file sizes for the footer (`N files · NN KB`). */
export function memoryFilesFooter(files: MemoryFileRef[]): {
  count: number;
  totalBytes: number;
} {
  return {
    count: files.length,
    totalBytes: files.reduce((sum, f) => sum + f.sizeBytes, 0),
  };
}

/** Format bytes for the files footer — always KB at this scale. */
export function formatMemoryFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${Math.round(bytes / 1024)} KB`;
}
