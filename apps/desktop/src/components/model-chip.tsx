import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Check, ChevronDown, Cpu, Search } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuCheckboxItem,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import { filterModels, FILTER_THRESHOLD } from "@/lib/model-filter";
import { useModelConfigStore, isLocalKind } from "@/store/model-config";
import { useProfilesStore } from "@/store/profiles";
import { useSessionModelStore } from "@/store/session-model";
import { ModelWindowInfo } from "@/components/model-window-info";

// Per-pane model chip (RFC 0005 §11.4, Phase D; #499). Shows the resolved model
// for this session and opens a quick picker (connection → model) that writes a
// session-scoped override; clearing falls back to the phenotype/global tiers.
// Session-scoped (like ModePill / WorkspaceSelector / ContextGauge), so each
// tiling pane (#148) selects independently. The backend resolves authoritatively
// (session > phenotype > global); this only reflects + sets it.
export function ModelChip({ sessionId }: { sessionId: string }) {
  const registry = useModelConfigStore((s) => s.registry);
  const modelsById = useModelConfigStore((s) => s.modelsById);
  const loadModels = useModelConfigStore((s) => s.loadModels);
  const loadRegistry = useModelConfigStore((s) => s.load);
  const registryLoading = useModelConfigStore((s) => s.loading);
  // Inline reasoning toggle for local models (#633): reads/writes the resolved
  // connection's `thinking` via the existing per-connection action — no new IPC.
  const setThinking = useModelConfigStore((s) => s.setThinking);
  const saving = useModelConfigStore((s) => s.saving);

  // Resolution depends on the global active connection + the active phenotype, so
  // re-resolve when either changes (the picker's own set/clear reloads directly).
  const phenoActiveId = useProfilesStore((s) => s.activeId);

  const resolved = useSessionModelStore((s) => s.resolvedBySession[sessionId]);
  const override = useSessionModelStore(
    (s) => s.overrideBySession[sessionId] ?? null,
  );
  const unavailable = useSessionModelStore(
    (s) => s.unavailableBySession[sessionId] ?? false,
  );
  // Served context window + source for this session (#602). Absent until the
  // backend forwards it; drives the dropdown readout and the under-fill warning dot.
  const servedWindow = useSessionModelStore(
    (s) => s.servedWindowBySession[sessionId],
  );
  const load = useSessionModelStore((s) => s.load);
  const setSelection = useSessionModelStore((s) => s.set);
  const clear = useSessionModelStore((s) => s.clear);

  // Hydrate the registry once (Settings also loads it, but the composer shouldn't
  // depend on Settings being opened). The shared store means only the first pane
  // triggers the fetch.
  useEffect(() => {
    if (!registry && !registryLoading) void loadRegistry();
  }, [registry, registryLoading, loadRegistry]);

  useEffect(() => {
    void load(sessionId);
  }, [sessionId, load, registry?.active, phenoActiveId]);

  // The backend resolver is unavailable for this session (e.g. the Phase D commands
  // aren't registered yet — this FE can merge ahead of its backend half). Hide the
  // chip entirely rather than show a permanently spinning / non-functional control;
  // it reappears once `load` succeeds. All hooks above run unconditionally first.
  if (unavailable) return null;

  const connections = registry?.connections ?? [];
  const resolvedConn = connections.find((c) => c.id === resolved?.connection);

  // Lazily fetch each connection's model list when the picker opens.
  const onOpenChange = (open: boolean) => {
    if (!open) return;
    for (const c of connections) {
      if (!modelsById[c.id]) void loadModels(c.id);
    }
  };

  return (
    <DropdownMenu onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          title={
            resolved
              ? `Model: ${resolvedConn?.displayName ?? resolved.connection} · ${resolved.model}${override ? " (session override)" : " (inherited)"}`
              : "Model"
          }
          aria-label="Session model"
          className="h-6 max-w-[40%] gap-1 px-1.5 text-xs font-medium text-muted-foreground hover:text-foreground"
        >
          <Cpu className="size-3.5 shrink-0" />
          {resolved ? (
            <span className="min-w-0 truncate">{resolved.model}</span>
          ) : (
            <Spinner className="shrink-0" />
          )}
          {/* A dot marks a session-scoped override vs an inherited selection. */}
          {override ? (
            <span
              aria-hidden
              className="size-1.5 shrink-0 rounded-full bg-primary"
            />
          ) : null}
          {/* Always-visible under-fill warning (#602): the served window fell back to
              the conservative default — likely a mis-set OLLAMA_CONTEXT_LENGTH — so
              flag it here rather than letting it pass silently. */}
          {servedWindow?.source === "default" ? (
            <span
              role="img"
              className="size-1.5 shrink-0 rounded-full bg-amber-500"
              title="Context window not detected — using the conservative default. Set OLLAMA_CONTEXT_LENGTH or FLOWFORGE_OLLAMA_NUM_CTX."
              aria-label="Context window not detected — using the conservative default"
            />
          ) : null}
          <ChevronDown className="size-3 shrink-0 opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-48">
        {/* Served context window + source (#602), shown above the picker so the
            user can see what window the active model is actually serving. */}
        {servedWindow ? (
          <>
            <div className="px-2 py-1.5">
              <ModelWindowInfo info={servedWindow} />
            </div>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {/* Inline "Thinking" toggle for local models (#633) — surfaces the existing
            per-connection reasoning switch (also in Settings → Model) right where the
            model is picked, so the speed↔reasoning tradeoff on CPU is one click away.
            A `CheckboxItem` (not a Switch nested in a plain Item) keeps the row
            keyboard-operable inside the menu — Enter/Space toggles it, arrow keys reach
            it — and exposes `menuitemcheckbox` / `aria-checked` to assistive tech.
            `onSelect`-preventDefault keeps the menu open; the Switch is a presentational
            mirror of the checked state (aria-hidden, non-interactive). */}
        {resolvedConn && isLocalKind(resolvedConn.kind) ? (
          <>
            <DropdownMenuCheckboxItem
              aria-label="Thinking"
              checked={resolvedConn.thinking}
              disabled={saving}
              onCheckedChange={(v) => void setThinking(resolvedConn.id, v)}
              onSelect={(e) => e.preventDefault()}
              className="flex items-start justify-between gap-3"
            >
              <span className="flex min-w-0 flex-col">
                <span className="text-[13px] font-medium text-foreground">
                  Thinking
                </span>
                <span className="text-[11px] leading-snug text-muted-foreground">
                  Off is faster on local models; on for hard tasks.
                </span>
              </span>
              <Switch
                aria-hidden
                tabIndex={-1}
                checked={resolvedConn.thinking}
                className="pointer-events-none"
              />
            </DropdownMenuCheckboxItem>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {connections.length === 0 ? (
          <DropdownMenuItem disabled>
            {registryLoading ? "Loading…" : "No connections"}
          </DropdownMenuItem>
        ) : (
          connections.map((c) => {
            const models = modelsById[c.id];
            return (
              <DropdownMenuSub key={c.id}>
                <DropdownMenuSubTrigger>
                  <Check
                    className={cn(
                      c.id === resolved?.connection
                        ? "opacity-100"
                        : "opacity-0",
                    )}
                  />
                  <span className="min-w-0 truncate">{c.displayName}</span>
                </DropdownMenuSubTrigger>
                <ModelList
                  models={models}
                  selectedModel={
                    c.id === resolved?.connection ? resolved.model : null
                  }
                  onPick={(m) =>
                    void setSelection(sessionId, {
                      connection: c.id,
                      model: m,
                    })
                  }
                />
              </DropdownMenuSub>
            );
          })
        )}
        <DropdownMenuSeparator />
        {/* Clearing the override returns the pane to the phenotype/global tiers. */}
        <DropdownMenuItem
          disabled={!override}
          onSelect={() => void clear(sessionId)}
        >
          <span className="text-muted-foreground">
            Use phenotype / global default
          </span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * One provider's models, with a search box (#1301).
 *
 * Lives inside that connection's submenu, so the filter is scoped to the
 * provider the user is pointing at — a search across every provider at once
 * would flatten the connection → models shape the picker is built on.
 *
 * Rows are plain options rather than `DropdownMenuItem`s, and the box owns the
 * keyboard. That is not a style choice: a Radix menu implements type-to-select
 * and roving focus on its content, so an input nested in one has its keystrokes
 * read as menu typeahead and its arrows steal focus onto items. Every keydown
 * that means something here is handled and stopped before the menu sees it.
 */
function ModelList({
  models,
  selectedModel,
  onPick,
}: {
  models: string[] | undefined;
  selectedModel: string | null;
  onPick: (model: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // Ids for the combobox wiring below. Per instance (`useId`), because several
  // providers' lists can be mounted at once and `aria-activedescendant` has to
  // resolve to a row in *this* list.
  const listId = useId();
  const optionId = (i: number) => `${listId}-option-${i}`;

  const results = useMemo(
    () => filterModels(query, models ?? []),
    [query, models],
  );

  // Clamp at read time rather than storing — results shrink as the user types,
  // and a stale `selected` must not need a reset effect to stay in range
  // (mirrors `palette.tsx`).
  const activeIndex = results.length
    ? Math.min(selected, results.length - 1)
    : 0;

  // Filtering a handful of models costs a step and saves nothing, so the box
  // only appears once a catalog is big enough to be a problem.
  const showFilter = (models?.length ?? 0) >= FILTER_THRESHOLD;

  // Take focus when the submenu opens. Radix leaves focus on the *trigger item*
  // for a hover-opened submenu, and a keystroke landing there is read as menu
  // typeahead — which jumps to another provider and collapses this submenu, so
  // "hover, then type" silently fails. The box has to claim focus, twice over:
  // once when it mounts, and again when the pointer arrives, because moving the
  // mouse across the trigger on the way in re-focuses that item.
  useEffect(() => {
    if (!showFilter) return;
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [showFilter]);

  const focusInput = () => inputRef.current?.focus();

  // Keep the highlighted row visible during arrow navigation.
  useEffect(() => {
    listRef.current
      ?.querySelector(`[data-index="${activeIndex}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  // First Escape clears a non-empty query; only a second one closes the menu.
  //
  // This has to go through Radix's own hook rather than `stopPropagation` in
  // the keydown handler: its dismiss layer listens on `document` in the
  // *capture* phase, so it runs before any React handler and the whole menu
  // closed on the first Escape.
  function onEscapeKeyDown(e: KeyboardEvent): void {
    if (!query) return;
    e.preventDefault();
    setQuery("");
    setSelected(0);
  }

  function onKeyDown(e: React.KeyboardEvent): void {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      // Stopped, or Radix moves focus to a menu item and the next character
      // typed goes to the menu's typeahead instead of the box.
      e.preventDefault();
      e.stopPropagation();
      const step = e.key === "ArrowDown" ? 1 : -1;
      setSelected(
        results.length
          ? (activeIndex + step + results.length) % results.length
          : 0,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      const hit = results[activeIndex];
      if (hit) onPick(hit);
    } else if (e.key.length === 1) {
      // A printable character: keep it in the box. Unstopped, the menu reads it
      // as typeahead and jumps the highlight to a matching item.
      e.stopPropagation();
    }
  }

  // A provider with only a handful of models keeps the list it always had: real
  // `DropdownMenuItem`s, which Radix makes arrow-navigable and Enter-selectable
  // for free. The searchable list below gives that up deliberately — its rows
  // are options driven by the search box — and with no box to drive them, the
  // trade would leave a small list unreachable by keyboard. Most providers list
  // fewer than `FILTER_THRESHOLD` models, so that would be a regression on the
  // common path, not an edge case (#1302 review).
  if (!showFilter) {
    return (
      <DropdownMenuSubContent className="max-h-72 max-w-64 overflow-y-auto">
        {models === undefined ? (
          <DropdownMenuItem disabled>Loading…</DropdownMenuItem>
        ) : models.length === 0 ? (
          <DropdownMenuItem disabled>No models</DropdownMenuItem>
        ) : (
          models.map((model) => (
            <DropdownMenuItem key={model} onSelect={() => onPick(model)}>
              <Check
                className={cn(
                  model === selectedModel ? "opacity-100" : "opacity-0",
                )}
              />
              <span className="min-w-0 truncate">{model}</span>
            </DropdownMenuItem>
          ))
        )}
      </DropdownMenuSubContent>
    );
  }

  return (
    <DropdownMenuSubContent
      onEscapeKeyDown={onEscapeKeyDown}
      onPointerEnter={focusInput}
      className="flex w-72 flex-col p-0"
    >
      <div className="flex items-center gap-2 border-b px-2.5">
        <Search className="size-3.5 shrink-0 text-muted-foreground" />
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setSelected(0); // typing always re-highlights the top result
          }}
          onKeyDown={onKeyDown}
          placeholder="Search models…"
          aria-label="Search models"
          // Combobox wiring (#1302 review): the rows are plain options driven
          // from here, so without this a screen reader never hears which one the
          // arrow keys moved to.
          role="combobox"
          aria-expanded
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={
            results.length ? optionId(activeIndex) : undefined
          }
          spellCheck={false}
          autoComplete="off"
          className="h-9 min-w-0 flex-1 bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground/50"
        />
        {/* What the filter did, so a short list reads as "filtered" rather
              than "that is all there is". */}
        <span className="shrink-0 text-[11px] text-muted-foreground/70">
          {query.trim()
            ? `${results.length} of ${models?.length ?? 0}`
            : (models?.length ?? 0)}
        </span>
      </div>

      <div
        ref={listRef}
        id={listId}
        role="listbox"
        aria-label="Models"
        onKeyDown={onKeyDown}
        className="max-h-72 overflow-y-auto p-1"
      >
        {models === undefined ? (
          <p className="px-2 py-3 text-[13px] text-muted-foreground">
            Loading…
          </p>
        ) : results.length === 0 ? (
          <p className="px-2 py-6 text-center text-[13px] text-muted-foreground/70">
            {query.trim() ? `No models match “${query.trim()}”` : "No models"}
          </p>
        ) : (
          results.map((model, i) => {
            const active = i === activeIndex;
            return (
              <div
                key={model}
                id={optionId(i)}
                data-index={i}
                role="option"
                aria-selected={active}
                title={model}
                onMouseMove={() => setSelected((cur) => (cur === i ? cur : i))}
                onClick={() => onPick(model)}
                className={cn(
                  "flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-[13px] select-none",
                  active
                    ? "bg-accent text-accent-foreground"
                    : "text-foreground/90",
                )}
              >
                <Check
                  className={cn(
                    "size-3.5 shrink-0",
                    model === selectedModel ? "opacity-100" : "opacity-0",
                  )}
                />
                <span className="min-w-0 flex-1 truncate">{model}</span>
              </div>
            );
          })
        )}
      </div>
    </DropdownMenuSubContent>
  );
}
