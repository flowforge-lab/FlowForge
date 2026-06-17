import { useEffect, useMemo } from "react";
import { FileText, Search } from "lucide-react";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import {
  filterMemory,
  formatMemoryFileSize,
  memoryFilesFooter,
  memorySnapshotIsEmpty,
  type MemoryCategoryContent,
  type MemoryJournalEntry,
  type MemoryFileRef,
} from "@/lib/memory";
import { useMemoryStore } from "@/store/memory";
import { useSettingsStore } from "@/store/settings";

const CATEGORY_META = [
  { key: "who" as const, label: "WHO" },
  { key: "how" as const, label: "HOW" },
  { key: "what" as const, label: "WHAT" },
];

/**
 * Memory section (SET.8, RFC 0002). Reference-IA browser over provisional mock
 * types — category cards, journal list, and indexed files with client-side search.
 * Shapes in `lib/memory.ts` are not backend contracts.
 */
export function MemorySection() {
  const snapshot = useMemoryStore((s) => s.snapshot);
  const query = useMemoryStore((s) => s.query);
  const loading = useMemoryStore((s) => s.loading);
  const error = useMemoryStore((s) => s.error);
  const load = useMemoryStore((s) => s.load);
  const setQuery = useMemoryStore((s) => s.setQuery);
  const resetMemory = useMemoryStore((s) => s.resetMemory);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    registerResetHandler(() => resetMemory());
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetMemory]);

  const filtered = useMemo(
    () => (snapshot ? filterMemory(snapshot, query) : null),
    [snapshot, query],
  );

  const filesFooter = filtered
    ? memoryFilesFooter(filtered.files)
    : { count: 0, totalBytes: 0 };

  const showEmptySearch =
    filtered !== null && query.trim() !== "" && memorySnapshotIsEmpty(filtered);

  return (
    <div className="space-y-5">
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        Browse ambient memory (RFC 0002). Layout and types are{" "}
        <strong className="font-medium text-foreground">provisional</strong> —
        mock data only until the memory backend lands.
      </p>

      <div className="relative">
        <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          placeholder="Search memory…"
          autoComplete="off"
          className="pl-8"
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}

      {loading && !snapshot ? (
        <p className="text-[12px] text-muted-foreground">Loading memory…</p>
      ) : showEmptySearch ? (
        <p className="text-[12px] text-muted-foreground">
          No memory matches &ldquo;{query.trim()}&rdquo;.
        </p>
      ) : filtered ? (
        <>
          <section className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            {CATEGORY_META.map(({ key, label }) => (
              <CategoryCard
                key={key}
                label={label}
                category={filtered.categories[key]}
              />
            ))}
          </section>

          <JournalList entries={filtered.journal} />

          <FilesList
            files={filtered.files}
            count={filesFooter.count}
            totalBytes={filesFooter.totalBytes}
          />
        </>
      ) : null}
    </div>
  );
}

function CategoryCard({
  label,
  category,
}: {
  label: string;
  category: MemoryCategoryContent;
}) {
  const visible = category.items.length > 0;
  return (
    <article
      className={cn(
        "flex flex-col gap-2 rounded-md border px-3 py-2.5",
        !visible && "opacity-50",
      )}
    >
      <div>
        <h4 className="text-[11px] font-semibold tracking-wide text-foreground">
          {label}
        </h4>
        <p className="text-[10px] text-muted-foreground">{category.subtitle}</p>
      </div>
      {visible ? (
        <ul className="space-y-1">
          {category.items.map((item) => (
            <li
              key={item}
              className="text-[11px] leading-relaxed text-muted-foreground"
            >
              {item}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-[11px] text-muted-foreground">—</p>
      )}
    </article>
  );
}

function JournalList({ entries }: { entries: MemoryJournalEntry[] }) {
  return (
    <section className="space-y-2 border-t pt-4">
      <h4 className="text-[11px] font-semibold tracking-wide text-foreground">
        JOURNAL
      </h4>
      {entries.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">
          No journal entries yet
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {entries.map((entry) => (
            <li
              key={entry.id}
              className="rounded-md border px-3 py-2.5 text-[12px]"
            >
              <time
                dateTime={entry.date}
                className="text-[10px] tabular-nums text-muted-foreground"
              >
                {entry.date}
              </time>
              <p className="mt-1 leading-relaxed text-foreground">
                {entry.content}
              </p>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function FilesList({
  files,
  count,
  totalBytes,
}: {
  files: MemoryFileRef[];
  count: number;
  totalBytes: number;
}) {
  return (
    <section className="space-y-2 border-t pt-4">
      <h4 className="text-[11px] font-semibold tracking-wide text-foreground">
        FILES
      </h4>
      {files.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">No files indexed.</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {files.map((file) => (
            <li
              key={file.name}
              className="flex items-center gap-2 rounded-md border px-3 py-2 text-[12px]"
            >
              <FileText
                className="size-3.5 shrink-0 text-muted-foreground"
                aria-hidden
              />
              <span className="min-w-0 flex-1 truncate text-foreground">
                {file.name}
              </span>
              <span className="shrink-0 tabular-nums text-[10px] text-muted-foreground">
                {formatMemoryFileSize(file.sizeBytes)}
              </span>
            </li>
          ))}
        </ul>
      )}
      <p className="text-[10px] tabular-nums text-muted-foreground">
        {count} file{count === 1 ? "" : "s"} ·{" "}
        {formatMemoryFileSize(totalBytes)}
      </p>
    </section>
  );
}
