import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
} from "react";
import {
  CircleOff,
  CornerDownLeft,
  Layers,
  MessageSquare,
  PanelRight,
  Plus,
  Search,
  Server,
  Sparkles,
  TextCursorInput,
  WrapText,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { ipc } from "@/lib/ipc";
import type { SkillInfo } from "@/bindings";
import { useChatStore } from "@/store/chat";
import { useSplitStore } from "@/store/split";
import { useSkillsStore } from "@/store/skills";
import { useMcpStore } from "@/store/mcp";
import { useSettingsStore } from "@/store/settings";
import {
  usePaletteStore,
  type PaletteCommand,
  type PaletteCommandKind,
} from "@/store/palette";
import {
  buildCommands,
  buildMcpServerCommands,
  buildPhenotypeCommands,
  buildSkillCommands,
  mergePaletteResults,
} from "@/lib/palette";

// ── Icons ─────────────────────────────────────────────────────────────────────
// Kept out of store/palette.ts (which stays pure data). Exhaustive by type: a new
// command kind without an icon is a compile error here too.
const ICONS: Record<
  PaletteCommandKind,
  ComponentType<{ className?: string }>
> = {
  "new-session": Plus,
  "switch-session": MessageSquare,
  "toggle-split": PanelRight,
  "toggle-wrap": WrapText,
  "focus-composer": TextCursorInput,
  "activate-skill": Sparkles,
  "deactivate-skill": CircleOff,
  "switch-phenotype": Layers,
  "open-mcp-server": Server,
};

// ── Command execution ─────────────────────────────────────────────────────────
// The exhaustive switch the issue calls for: adding a PaletteCommand kind without
// a matching arm is a compile error (the never-guard finds you). Each arm reuses
// an existing store action — skill/phenotype arms call ipc (#27 / #28).

function focusComposer(): void {
  // Defer a frame so the overlay has unmounted before focus moves to the textarea.
  requestAnimationFrame(() => {
    document.querySelector<HTMLTextAreaElement>("[data-composer]")?.focus();
  });
}

function runCommand(cmd: PaletteCommand): void {
  switch (cmd.kind) {
    case "new-session":
      void useChatStore.getState().newSession();
      return;
    case "switch-session":
      void useChatStore.getState().selectSession(cmd.sessionId);
      return;
    case "toggle-split":
      useSplitStore.getState().toggleSplit();
      return;
    case "toggle-wrap":
      useSplitStore.getState().toggleWrap();
      return;
    case "focus-composer":
      focusComposer();
      return;
    case "activate-skill":
      void ipc.activateSkill(cmd.name);
      return;
    case "deactivate-skill":
      void ipc.deactivateSkill(cmd.name);
      return;
    case "switch-phenotype":
      // skills:changed from the backend triggers refresh via events.ts.
      void ipc.switchPhenotype(cmd.name);
      return;
    case "open-mcp-server":
      // Open the MCP settings section to manage servers. (Per-server focus from
      // the chosen `serverId` is a follow-up; the section lists all servers.)
      useSettingsStore.getState().setSection("mcp");
      useSettingsStore.getState().openSettings();
      return;
    default: {
      // Exhaustiveness guard: a new PaletteCommand kind without a case above
      // becomes a compile error here. See store/palette.ts.
      const unreachable: never = cmd;
      return unreachable;
    }
  }
}

// ── Overlay ───────────────────────────────────────────────────────────────────

// Thin wrapper so the body mounts fresh each open: query/selection reset for
// free (no effect), and the palette costs nothing while closed.
export function CommandPalette() {
  const open = usePaletteStore((s) => s.open);
  if (!open) return null;
  return <PaletteBody />;
}

function PaletteBody() {
  const closePalette = usePaletteStore((s) => s.closePalette);
  const pushRecent = usePaletteStore((s) => s.pushRecent);
  const recent = usePaletteStore((s) => s.recent);

  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const sessionTitles = useChatStore((s) => s.sessionTitles);

  const phenotypes = useSkillsStore((s) => s.phenotypes);
  const activePhenotype = useSkillsStore((s) => s.activePhenotype);
  const refreshSkills = useSkillsStore((s) => s.refresh);
  const searchSkills = useSkillsStore((s) => s.search);

  const mcpServers = useMcpStore((s) => s.servers);
  const loadMcp = useMcpStore((s) => s.load);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [skillHits, setSkillHits] = useState<SkillInfo[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Refresh installed skills + phenotypes + MCP servers each time the palette opens.
  useEffect(() => {
    void refreshSkills();
    void loadMcp();
  }, [refreshSkills, loadMcp]);

  // Rank skill hits via the backend search contract (#27).
  useEffect(() => {
    let cancelled = false;
    void searchSkills(query.trim()).then((hits) => {
      if (!cancelled) setSkillHits(hits);
    });
    return () => {
      cancelled = true;
    };
  }, [query, searchSkills]);

  const staticCommands = useMemo(
    () => [
      ...buildCommands({ sessions, activeSessionId, sessionTitles }),
      ...buildPhenotypeCommands({ phenotypes, activePhenotype }),
      ...buildMcpServerCommands(mcpServers),
    ],
    [
      sessions,
      activeSessionId,
      sessionTitles,
      phenotypes,
      activePhenotype,
      mcpServers,
    ],
  );

  const skillCommands = useMemo(
    () => buildSkillCommands(skillHits),
    [skillHits],
  );

  const results = useMemo(
    () => mergePaletteResults(staticCommands, skillCommands, query, recent),
    [staticCommands, skillCommands, query, recent],
  );

  // Clamp at read time rather than storing — results can shrink under a stale
  // `selected` (typing, or sessions changing while open) without a reset effect.
  const activeIndex = results.length
    ? Math.min(selected, results.length - 1)
    : 0;

  // Focus the input on mount (DOM sync, not state) — the wrapper mounts us anew
  // each open, so this runs exactly when the palette appears.
  useEffect(() => {
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, []);

  // Keep the highlighted row visible during arrow navigation.
  useEffect(() => {
    listRef.current
      ?.querySelector(`[data-index="${activeIndex}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  function run(cmd: PaletteCommand): void {
    closePalette();
    pushRecent(cmd.id);
    runCommand(cmd);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>): void {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected(results.length ? (activeIndex + 1) % results.length : 0);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected(
        results.length
          ? (activeIndex - 1 + results.length) % results.length
          : 0,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = results[activeIndex];
      if (cmd) run(cmd);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closePalette();
    }
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      className="fixed inset-0 z-50 flex items-start justify-center"
    >
      {/* Click-outside closes. Separate element so a click on the panel (a
          sibling painted above) never reaches it. */}
      <div
        className="absolute inset-0 bg-background/60 backdrop-blur-sm"
        onMouseDown={closePalette}
      />

      <div className="relative mt-[12vh] flex max-h-[70vh] w-[92%] max-w-xl flex-col overflow-hidden rounded-xl border bg-card shadow-2xl">
        {/* Search input */}
        <div className="flex items-center gap-2.5 border-b px-3.5">
          <Search className="size-4 shrink-0 text-muted-foreground" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(0); // typing always re-highlights the top result
            }}
            onKeyDown={onKeyDown}
            placeholder="Search commands, sessions, skills…"
            spellCheck={false}
            autoComplete="off"
            className="h-11 flex-1 bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground/50"
          />
        </div>

        {/* Results */}
        <div
          ref={listRef}
          role="listbox"
          className="min-h-0 flex-1 overflow-y-auto p-1.5"
        >
          {results.length === 0 ? (
            <div className="px-3 py-8 text-center text-[13px] text-muted-foreground/70">
              No commands match “{query.trim()}”
            </div>
          ) : (
            results.map((cmd, i) => {
              const Icon = ICONS[cmd.kind];
              const active = i === activeIndex;
              return (
                <div
                  key={cmd.id}
                  data-index={i}
                  role="option"
                  aria-selected={active}
                  onMouseMove={() =>
                    setSelected((cur) => (cur === i ? cur : i))
                  }
                  onClick={() => run(cmd)}
                  className={cn(
                    "flex cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-2 text-[13px] select-none",
                    active
                      ? "bg-accent text-accent-foreground"
                      : "text-foreground/90",
                  )}
                >
                  <Icon className="size-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{cmd.title}</span>
                  {cmd.hint && (
                    <kbd className="shrink-0 font-mono text-[11px] text-muted-foreground/60">
                      {cmd.hint}
                    </kbd>
                  )}
                </div>
              );
            })
          )}
        </div>

        {/* Footer key hints */}
        <div className="flex shrink-0 items-center gap-3 border-t px-3.5 py-2 text-[11px] text-muted-foreground/60">
          <span className="flex items-center gap-1">
            <kbd className="font-mono">↑</kbd>
            <kbd className="font-mono">↓</kbd>
            navigate
          </span>
          <span className="flex items-center gap-1">
            <CornerDownLeft className="size-3" />
            select
          </span>
          <span className="flex items-center gap-1">
            <kbd className="font-mono">esc</kbd>
            close
          </span>
        </div>
      </div>
    </div>
  );
}
