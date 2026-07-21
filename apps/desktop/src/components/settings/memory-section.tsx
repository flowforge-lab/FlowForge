import { useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronLeft,
  Pencil,
  Pin,
  RotateCcw,
  Search,
  Sparkles,
  X,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { MarkdownEditor } from "@/components/ui/markdown-editor";
import { Progress } from "@/components/ui/progress";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useSettingsStore } from "@/store/settings";
import { useMemoryStore } from "@/store/memory";
import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";
import {
  MEMORY_CATEGORY_META,
  buildFiles,
  buildJournal,
  categoryMatches,
  clampCategoryBody,
  filterChunks,
  filterFiles,
  filterJournal,
  formatBytes,
  formatMemoryFooter,
  humanAge,
  matchingCategoryIds,
  parseCategories,
  sortChunks,
  weightPercent,
  type MemoryCategories,
  type MemoryCategoryId,
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

/**
 * One category tab's pane (#906): a clamped preview of the curated body, with
 * "See more" only when the body is actually truncated. No inner scrollbar —
 * the Memory pane already lives inside the Settings dialog's one `ScrollArea`,
 * so a second nested scroll region is exactly the bug this replaces (#903).
 *
 * Edit (#868) is always offered, including for a short body that never
 * truncates; it opens the same reading view "See more" does, already in edit
 * mode, so the editor gets the full pane rather than growing this card.
 */
function CategoryPane({
  subtitle,
  body,
  query,
  onSeeMore,
  onEdit,
}: {
  subtitle: string;
  body: string;
  query: string;
  onSeeMore: () => void;
  onEdit: () => void;
}) {
  const hidden = query.trim() !== "" && !categoryMatches(body, query);
  const { preview, truncated } = clampCategoryBody(body);
  return (
    <div className="rounded-md border border-border bg-muted/40 p-3">
      <div className="text-[11px] text-muted-foreground">{subtitle}</div>
      <div className="mt-2 whitespace-pre-wrap text-[12px] leading-relaxed text-foreground/90">
        {body === "" ? (
          <span className="text-muted-foreground/70">No entries yet</span>
        ) : hidden ? (
          <span className="text-muted-foreground/70">No match</span>
        ) : (
          preview
        )}
      </div>
      <div className="mt-2 flex items-center gap-3">
        {!hidden && truncated ? (
          <button
            type="button"
            onClick={onSeeMore}
            className="text-[11px] font-medium text-primary hover:underline"
          >
            See more
          </button>
        ) : null}
        <Button
          size="xs"
          variant="ghost"
          className="ml-auto"
          disabled={hidden}
          onClick={onEdit}
        >
          <Pencil />
          Edit
        </Button>
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
 * One Salience row (M6.2, #293): a memory chunk with its effective weight, a
 * dormant badge, and the wake/pin controls. `weight` and `dormant` are rendered
 * verbatim from the backend (it owns the threshold); reset/pin route through the
 * store, which re-pulls the authoritative snapshot. Decay never edits Markdown.
 */
function ChunkRow({
  chunk,
  busy,
  onReset,
  onPin,
}: {
  chunk: MemoryChunkStat;
  busy: boolean;
  onReset: () => void;
  onPin: (pinned: boolean) => void;
}) {
  const pct = weightPercent(chunk.weight);
  const title = chunk.heading ?? chunk.relPath;
  return (
    <div className="rounded-md border border-border bg-muted/40 px-2.5 py-2">
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-foreground">
          {title}
        </span>
        {chunk.pinned ? <Badge tone="sky">pinned</Badge> : null}
        {chunk.dormant ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <Badge tone="amber" tabIndex={0}>
                dormant
              </Badge>
            </TooltipTrigger>
            <TooltipContent className="max-w-56">
              Decayed from disuse — skipped from ambient injection to save
              tokens, but still searchable and never deleted. Wake or pin to
              restore it.
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>

      {chunk.preview ? (
        <p className="mt-0.5 truncate text-[12px] text-muted-foreground">
          {chunk.preview}
        </p>
      ) : null}

      <div className="mt-1.5 flex items-center gap-2">
        <Progress
          value={pct}
          aria-label={`Weight ${pct}%`}
          className="h-1.5 flex-1"
        />
        <span className="w-9 shrink-0 text-right text-[10px] tabular-nums text-muted-foreground">
          {pct}%
        </span>
      </div>

      <div className="mt-1.5 flex items-center gap-2 text-[11px] text-muted-foreground">
        <span className="truncate">
          {humanAge(chunk.lastAccessedMs)}
          {chunk.accessCount > 0 ? ` · recalled ${chunk.accessCount}×` : null}
        </span>
        <label className="ml-auto flex shrink-0 cursor-pointer items-center gap-1.5">
          <Pin className="size-3 text-muted-foreground" />
          <span className="sr-only">Pin {title}</span>
          <Switch
            checked={chunk.pinned}
            onCheckedChange={onPin}
            disabled={busy}
            aria-label={`Pin ${title}`}
          />
        </label>
        <Button
          size="xs"
          variant="ghost"
          disabled={busy || chunk.weight >= 1}
          onClick={onReset}
          title="Wake — reset weight to full"
        >
          <RotateCcw />
          Wake
        </Button>
      </div>
    </div>
  );
}

/**
 * Memory section (SET.8, #131): a browser over the RFC 0006 memory store.
 * Curated `MEMORY.md` parses into the Identity / Patterns / Focus cards
 * (RFC 0008 §3); `daily/*` files become the JOURNAL; every file is listed under
 * FILES with a count/size footer. Search filters all surfaces client-side; the
 * footer "Reset to defaults" clears it.
 *
 * The three curated strata are editable (#868) — Edit opens the reading view in
 * edit mode, and Save routes through the backend's whole-stratum replace command
 * rather than writing the file from the frontend. Everything else is read-only.
 */
export function MemorySection() {
  const overview = useMemoryStore((s) => s.overview);
  const files = useMemoryStore((s) => s.files);
  const curatedBody = useMemoryStore((s) => s.curatedBody);
  const journalBodies = useMemoryStore((s) => s.journalBodies);
  const chunks = useMemoryStore((s) => s.chunks);
  const chunkBusy = useMemoryStore((s) => s.chunkBusy);
  const resetChunk = useMemoryStore((s) => s.resetChunk);
  const setPinned = useMemoryStore((s) => s.setPinned);
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

  // Category tabs + in-modal reading view (#906).
  const [tab, setTab] = useState<MemoryCategoryId>("identity");
  const [reading, setReading] = useState<MemoryCategoryId | null>(null);
  const readingHeadingRef = useRef<HTMLHeadingElement>(null);
  const activeTriggerRef = useRef<HTMLButtonElement>(null);

  // Editing (#868). `draft` is the MarkdownEditor's buffer — the editor is
  // uncontrolled (it seeds `value` once on mount), so this holds the text to
  // save rather than driving the editor. `null` means "untouched since Edit".
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<string | null>(null);
  // Both destructive edits route through one confirm dialog: `discard` throws
  // away an unsaved buffer, `clear` writes an empty body (the backend keeps the
  // heading and drops the content), which select-all + delete + Save would
  // otherwise do silently.
  const [confirm, setConfirm] = useState<null | {
    kind: "discard" | "clear";
    action: () => void;
  }>(null);
  const writeBusy = useMemoryStore((s) => s.writeBusy);
  const writeStratum = useMemoryStore((s) => s.writeStratum);

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
    registerResetHandler(() => {
      resetSearch();
      setReading(null);
      setEditing(false);
      setDraft(null);
      setConfirm(null);
    });
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetSearch]);

  const categories: MemoryCategories = useMemo(
    () => parseCategories(curatedBody),
    [curatedBody],
  );
  const matchingIds: MemoryCategoryId[] = useMemo(
    () => matchingCategoryIds(categories, query),
    [categories, query],
  );

  // Auto-select the first matching tab while searching, unless the active tab
  // already matches. Done as a render-phase adjustment keyed on the memoized
  // `matchingIds` reference (React's recommended alternative to a setState
  // effect): it re-evaluates only when the query or curated bodies change, and
  // converges in one pass since setting `tab` to a match doesn't change
  // `matchingIds`. Leaves a manually-selected tab in place once it matches, and
  // whenever the query is cleared or nothing matches.
  const [prevMatchingIds, setPrevMatchingIds] = useState(matchingIds);
  if (matchingIds !== prevMatchingIds) {
    setPrevMatchingIds(matchingIds);
    if (
      query.trim() !== "" &&
      matchingIds.length > 0 &&
      !matchingIds.includes(tab)
    ) {
      setTab(matchingIds[0]);
    }
  }

  // Move focus into the reading view on entry — nothing else does (the
  // dialog's own focus effect only runs on mount/unmount) — which also
  // scrolls it into view via the browser's native focus-scroll behavior.
  useEffect(() => {
    if (reading !== null && !editing) readingHeadingRef.current?.focus();
  }, [reading, editing]);

  // Entering edit mode, focus the editor itself. Milkdown creates its view a
  // few microtasks after render, so poll for the ProseMirror surface over a
  // bounded number of frames rather than assuming it is there on commit.
  const editorHostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!editing) return;
    let frames = 0;
    let raf = 0;
    const tryFocus = () => {
      const el = editorHostRef.current?.querySelector<HTMLElement>(
        "[contenteditable='true']",
      );
      if (el) {
        el.focus();
        return;
      }
      if (frames++ < 30) raf = requestAnimationFrame(tryFocus);
    };
    raf = requestAnimationFrame(tryFocus);
    return () => cancelAnimationFrame(raf);
  }, [editing]);

  const journal: MemoryJournalEntry[] = useMemo(
    () => filterJournal(buildJournal(files, journalBodies), query),
    [files, journalBodies, query],
  );
  const fileRows: MemoryFileRef[] = useMemo(
    () => filterFiles(buildFiles(files), query),
    [files, query],
  );
  const chunkRows: MemoryChunkStat[] = useMemo(
    () => sortChunks(filterChunks(chunks, query)),
    [chunks, query],
  );

  // "See more" swaps the whole pane into a full-height reading view — the
  // Settings dialog has exactly one ScrollArea (owned by settings-shell, not
  // per-section), so this is the only way to give long-form prose the whole
  // max-h-[85vh] pane without a second, nested scrollbar (#903).
  if (reading !== null) {
    const meta = MEMORY_CATEGORY_META.find((m) => m.id === reading);
    const body = categories[reading];
    const hidden = query.trim() !== "" && !categoryMatches(body, query);

    // Milkdown re-serializes the Markdown it parsed (list markers, escapes,
    // trailing newline), so compare trimmed — opening and closing the editor
    // without typing must not register as an edit.
    const dirty = draft !== null && draft.trim() !== body.trim();

    const leaveEditor = () => {
      setEditing(false);
      setDraft(null);
    };
    const guard = (action: () => void) =>
      dirty ? setConfirm({ kind: "discard", action }) : action();

    const exitReading = () => {
      leaveEditor();
      setReading(null);
      requestAnimationFrame(() => activeTriggerRef.current?.focus());
    };

    const write = async () => {
      if (draft === null) return leaveEditor();
      const ok = await writeStratum(reading, draft);
      // On failure keep the buffer and stay in edit mode — the store's `error`
      // is already rendered above the pane, and the user's text is unsaved.
      if (ok) leaveEditor();
    };

    // Saving an empty body clears the stratum (the backend keeps only the
    // heading). That is a legitimate action, but it's one keystroke away from
    // select-all + delete, so confirm it — symmetric with the discard guard.
    // Only when there was something to lose: clearing an already-empty stratum
    // needs no ceremony.
    const save = () => {
      if (draft !== null && draft.trim() === "" && body.trim() !== "") {
        setConfirm({ kind: "clear", action: () => void write() });
        return;
      }
      void write();
    };

    return (
      <div className="space-y-3">
        <button
          type="button"
          onClick={() => guard(exitReading)}
          className="flex items-center gap-1 text-[12px] font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          <ChevronLeft className="size-3.5" />
          Back
        </button>
        <div className="flex items-start gap-2">
          <div className="min-w-0 flex-1">
            <h2
              ref={readingHeadingRef}
              tabIndex={-1}
              className="text-[13px] font-medium text-foreground outline-none"
            >
              {meta?.label}
            </h2>
            <p className="mt-0.5 text-[11px] text-muted-foreground">
              {meta?.subtitle}
            </p>
          </div>
          {editing ? null : (
            <Button size="xs" variant="ghost" onClick={() => setEditing(true)}>
              <Pencil />
              Edit
            </Button>
          )}
        </div>

        {error ? (
          <p className="text-[12px] text-destructive" role="alert">
            {error}
          </p>
        ) : null}

        {editing ? (
          <>
            <div ref={editorHostRef}>
              <MarkdownEditor
                // Uncontrolled: `value` seeds the document once on mount, so the
                // key remounts the editor when a different stratum is opened.
                key={reading}
                value={body}
                onChange={setDraft}
                placeholder="Nothing here yet — write what the agent should remember."
              />
            </div>
            <div className="flex items-center gap-2">
              <Button size="sm" disabled={!dirty || writeBusy} onClick={save}>
                {writeBusy ? "Saving…" : "Save"}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={writeBusy}
                onClick={() => guard(leaveEditor)}
              >
                Cancel
              </Button>
              <span className="text-[11px] text-muted-foreground">
                Replaces the whole {meta?.label} section of MEMORY.md.
              </span>
            </div>
          </>
        ) : (
          <div className="whitespace-pre-wrap text-[13px] leading-relaxed text-foreground/90">
            {body === "" ? (
              <span className="text-muted-foreground/70">No entries yet</span>
            ) : hidden ? (
              <span className="text-muted-foreground/70">No match</span>
            ) : (
              body
            )}
          </div>
        )}

        <AlertDialog
          open={confirm !== null}
          onOpenChange={(next) => !next && setConfirm(null)}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>
                {confirm?.kind === "clear"
                  ? `Clear the ${meta?.label} section?`
                  : "Discard unsaved changes?"}
              </AlertDialogTitle>
              <AlertDialogDescription>
                {confirm?.kind === "clear"
                  ? `Saving an empty editor removes everything under ${meta?.label} in MEMORY.md, keeping only the heading. The agent will no longer recall it.`
                  : `Your edits to ${meta?.label} have not been written to MEMORY.md. Discarding restores the version currently on disk.`}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep editing</AlertDialogCancel>
              <AlertDialogAction
                onClick={() => {
                  confirm?.action();
                  setConfirm(null);
                }}
              >
                {confirm?.kind === "clear" ? "Clear section" : "Discard"}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        The Markdown memory on disk
        {overview ? (
          <>
            {" at "}
            <code className="text-[11px]">{overview.rootPath}</code>
          </>
        ) : null}
        . Captured automatically — Identity, Patterns, and Focus can be edited
        here; the journal and other files are read-only.
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
          <Tabs
            value={tab}
            onValueChange={(v) => setTab(v as MemoryCategoryId)}
          >
            <TabsList aria-label="Memory categories">
              {MEMORY_CATEGORY_META.map((meta) => (
                <TabsTrigger
                  key={meta.id}
                  value={meta.id}
                  ref={meta.id === tab ? activeTriggerRef : undefined}
                >
                  {meta.label}
                  {query.trim() !== "" && matchingIds.includes(meta.id) ? (
                    <span
                      className="ml-1.5 inline-block size-1.5 rounded-full bg-primary"
                      aria-hidden
                    />
                  ) : null}
                </TabsTrigger>
              ))}
            </TabsList>
            <TabsContent value={tab}>
              <CategoryPane
                subtitle={
                  MEMORY_CATEGORY_META.find((m) => m.id === tab)?.subtitle ?? ""
                }
                body={categories[tab]}
                query={query}
                onSeeMore={() => setReading(tab)}
                onEdit={() => {
                  setDraft(null);
                  setEditing(true);
                  setReading(tab);
                }}
              />
            </TabsContent>
          </Tabs>

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

          <section className="space-y-2">
            <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              Salience
            </h3>
            <p className="text-[11px] leading-relaxed text-muted-foreground/80">
              Weight decays with disuse and is reinforced on recall. Dormant
              chunks are skipped from ambient injection to save tokens but stay
              searchable and are never deleted — wake or pin any chunk.
            </p>
            {chunkRows.length === 0 ? (
              <p className="text-[12px] text-muted-foreground">
                {query.trim() !== ""
                  ? "No chunks match your search."
                  : "No memory chunks yet."}
              </p>
            ) : (
              <TooltipProvider delayDuration={150}>
                <div className="space-y-1.5">
                  {chunkRows.map((chunk) => (
                    <ChunkRow
                      key={chunk.chunkKey}
                      chunk={chunk}
                      busy={chunkBusy[chunk.chunkKey] ?? false}
                      onReset={() => void resetChunk(chunk.chunkKey)}
                      onPin={(pinned) => void setPinned(chunk.chunkKey, pinned)}
                    />
                  ))}
                </div>
              </TooltipProvider>
            )}
          </section>
        </>
      )}
    </div>
  );
}
