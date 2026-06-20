import { useEffect, useMemo, useRef, useState } from "react";
import { Search, Sparkles, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { useSettingsStore } from "@/store/settings";
import { useMemoryStore } from "@/store/memory";
import {
  MEMORY_CATEGORY_META,
  buildFiles,
  buildJournal,
  categoryMatches,
  filterFiles,
  filterJournal,
  formatBytes,
  formatMemoryFooter,
  parseCategories,
  type MemoryCategories,
  type MemoryFileRef,
  type MemoryJournalEntry,
} from "@/lib/memory-view";

/** `2026-06-18` → `Jun 18, 2026`, parsed by parts to avoid a UTC day-shift. */
function prettyDate(iso: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  if (!m) return iso;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  }).format(d);
}

function CategoryCard({
  label,
  subtitle,
  body,
  query,
}: {
  label: string;
  subtitle: string;
  body: string;
  query: string;
}) {
  const hidden = query.trim() !== "" && !categoryMatches(body, query);
  return (
    <div className="rounded-md border border-border bg-muted/40 p-3">
      <div className="text-[12px] font-medium text-foreground">{label}</div>
      <div className="mt-0.5 text-[11px] text-muted-foreground">{subtitle}</div>
      <div className="mt-2 whitespace-pre-wrap text-[12px] leading-relaxed text-foreground/90">
        {body === "" ? (
          <span className="text-muted-foreground/70">No entries yet</span>
        ) : hidden ? (
          <span className="text-muted-foreground/70">No match</span>
        ) : (
          body
        )}
      </div>
    </div>
  );
}

function FileRow({ file }: { file: MemoryFileRef }) {
  return (
    <div className="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-2.5 py-1.5">
      <code className="min-w-0 flex-1 truncate text-[11px] text-foreground">
        {file.name}
      </code>
      <span
        className={cn(
          "rounded px-1.5 py-0.5 text-[10px] font-medium",
          file.kind === "curated"
            ? "bg-primary/10 text-primary"
            : "bg-muted text-muted-foreground",
        )}
      >
        {file.kind}
      </span>
      <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
        {formatBytes(file.sizeBytes)}
      </span>
    </div>
  );
}

/**
 * Memory section (SET.8, #131): a read-only browser over the RFC 0006 memory
 * store. Curated `MEMORY.md` parses into the Identity / Patterns / Focus cards
 * (RFC 0008 §3); `daily/*` files become the JOURNAL; every file is listed under
 * FILES with a count/size footer. Search filters all surfaces client-side; the
 * footer "Reset to defaults" clears it.
 */
export function MemorySection() {
  const overview = useMemoryStore((s) => s.overview);
  const files = useMemoryStore((s) => s.files);
  const curatedBody = useMemoryStore((s) => s.curatedBody);
  const journalBodies = useMemoryStore((s) => s.journalBodies);
  const query = useMemoryStore((s) => s.query);
  const loading = useMemoryStore((s) => s.loading);
  const error = useMemoryStore((s) => s.error);
  const load = useMemoryStore((s) => s.load);
  const setQuery = useMemoryStore((s) => s.setQuery);
  const resetSearch = useMemoryStore((s) => s.resetSearch);
  const flushCount = useMemoryStore((s) => s.flushCount);
  const lastFlushWrites = useMemoryStore((s) => s.lastFlushWrites);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  // Dismiss the provenance banner until the next flush bumps the count.
  const [dismissedFlush, setDismissedFlush] = useState(0);
  const showFlushBanner = flushCount > 0 && flushCount !== dismissedFlush;

  useEffect(() => {
    void load();
  }, [load]);

  // Re-pull files when a flush curates memory mid-session (#283) so freshly
  // written content shows without reopening the pane. Skip the initial render so
  // this doesn't race the mount load above when `flushCount` is already > 0 (the
  // pane opened after an earlier flush) — only *subsequent* flushes reload.
  const didMountFlushWatch = useRef(false);
  useEffect(() => {
    if (!didMountFlushWatch.current) {
      didMountFlushWatch.current = true;
      return;
    }
    if (flushCount > 0) void load();
  }, [flushCount, load]);

  useEffect(() => {
    registerResetHandler(() => resetSearch());
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetSearch]);

  const categories: MemoryCategories = useMemo(
    () => parseCategories(curatedBody),
    [curatedBody],
  );
  const journal: MemoryJournalEntry[] = useMemo(
    () => filterJournal(buildJournal(files, journalBodies), query),
    [files, journalBodies, query],
  );
  const fileRows: MemoryFileRef[] = useMemo(
    () => filterFiles(buildFiles(files), query),
    [files, query],
  );

  return (
    <div className="space-y-4">
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        Read-only view of the Markdown memory on disk
        {overview ? (
          <>
            {" at "}
            <code className="text-[11px]">{overview.rootPath}</code>
          </>
        ) : null}
        . Captured automatically; edit the files directly to change it.
      </p>

      {showFlushBanner ? (
        <div
          role="status"
          className="flex items-center gap-2 rounded-md border border-emerald-500/30 bg-emerald-500/10 px-2.5 py-1.5 text-[12px] text-emerald-700 dark:text-emerald-300"
        >
          <Sparkles className="size-3.5 shrink-0" />
          <span className="min-w-0 flex-1">
            Memory was auto-curated this session — {lastFlushWrites} new{" "}
            {lastFlushWrites === 1 ? "entry" : "entries"} from the latest flush.
          </span>
          <button
            type="button"
            aria-label="Dismiss"
            title="Dismiss"
            onClick={() => setDismissedFlush(flushCount)}
            className="shrink-0 rounded p-0.5 text-emerald-700/70 transition-colors hover:bg-emerald-500/15 hover:text-emerald-700 dark:text-emerald-300/70 dark:hover:text-emerald-200"
          >
            <X className="size-3.5" />
          </button>
        </div>
      ) : null}

      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}

      <div className="relative">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          aria-label="Search memory"
          type="search"
          value={query}
          placeholder="Search memory…"
          autoComplete="off"
          className="pl-8"
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {loading && files.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">Loading…</p>
      ) : (
        <>
          <section className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            {MEMORY_CATEGORY_META.map((meta) => (
              <CategoryCard
                key={meta.id}
                label={meta.label}
                subtitle={meta.subtitle}
                body={categories[meta.id]}
                query={query}
              />
            ))}
          </section>

          <section className="space-y-2">
            <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              Journal
            </h3>
            {journal.length === 0 ? (
              <p className="text-[12px] text-muted-foreground">
                No journal entries yet
              </p>
            ) : (
              <div className="space-y-1.5">
                {journal.map((entry) => (
                  <div
                    key={entry.relPath}
                    className="rounded-md border border-border bg-muted/40 px-2.5 py-1.5"
                  >
                    <div className="text-[11px] font-medium text-foreground">
                      {entry.date ? prettyDate(entry.date) : entry.relPath}
                    </div>
                    {entry.preview ? (
                      <div className="truncate text-[12px] text-muted-foreground">
                        {entry.preview}
                      </div>
                    ) : null}
                  </div>
                ))}
              </div>
            )}
          </section>

          <section className="space-y-2">
            <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              Files
            </h3>
            {fileRows.length === 0 ? (
              <p className="text-[12px] text-muted-foreground">
                {query.trim() !== ""
                  ? "No files match your search."
                  : "No files yet."}
              </p>
            ) : (
              <div className="space-y-1.5">
                {fileRows.map((file) => (
                  <FileRow key={file.relPath} file={file} />
                ))}
              </div>
            )}
            {overview ? (
              <p className="pt-1 text-[11px] text-muted-foreground/80">
                {formatMemoryFooter(overview.fileCount, overview.totalBytes)}
              </p>
            ) : null}
          </section>
        </>
      )}
    </div>
  );
}
