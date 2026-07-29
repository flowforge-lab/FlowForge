// ⌘K command palette state. The palette is the keyboard-native command surface
// ("Cmd/Ctrl+K is home", PRINCIPLES.md): a fuzzy-searchable registry of quick
// actions, sessions, skills, and phenotypes. Mirrors store/split.ts: a
// discriminated union of what the palette can do, plus a small zustand store for
// open/recents.

import { create } from "zustand";

// ── Command registry types ────────────────────────────────────────────────────

// Fields every command shares. `keywords` is folded into the fuzzy match so a
// command surfaces under synonyms the title doesn't contain; `hint` is the
// right-aligned affordance (a shortcut like ⌘N, or a context label).
interface CommandBase {
  /** Stable id — used for React keys and recents. */
  id: string;
  /** Label shown in the palette list. */
  title: string;
  /** Extra terms folded into the fuzzy match (synonyms, not displayed). */
  keywords?: string;
  /** Right-aligned affordance: a shortcut (⌘N) or a context label (Switch). */
  hint?: string;
}

// Discriminated union of everything the palette can run. Adding a command is one
// arm here + one matching arm in the exhaustive `switch` in palette.tsx — a new
// `kind` without a handler is a compile-time error (the never-guard finds you).
// Icon and execution live in palette.tsx (the consumer); this stays pure data,
// same split-of-concerns as store/split.ts ↔ split-panel.tsx.
export type PaletteCommand = CommandBase &
  (
    | { kind: "new-session" }
    | { kind: "split-pane-right" }
    | { kind: "split-pane-down" }
    | { kind: "switch-session"; sessionId: string }
    | { kind: "toggle-split" }
    | { kind: "toggle-wrap" }
    | { kind: "open-files" }
    | { kind: "focus-composer" }
    | { kind: "start-goal" }
    | { kind: "activate-skill"; name: string }
    | { kind: "deactivate-skill"; name: string }
    | { kind: "switch-phenotype"; name: string }
    | { kind: "open-mcp-server"; serverId: string }
  );

/** The set of command kinds — handy for building exhaustive icon/handler maps. */
export type PaletteCommandKind = PaletteCommand["kind"];

// ── Persistence ──────────────────────────────────────────────────────────────
// Only recents persist: the last few command ids, most-recent first, so an empty
// query surfaces what you reach for. `open` is ephemeral (never restore a modal
// across reloads). Mirrors the load/persist helpers in store/split.ts.
//
// Stored through `durableStorage` (#1134) — a WKWebView doesn't reliably flush
// localStorage before the process exits, so a command run late in a session
// could otherwise vanish from recents on quit. The on-disk shape is unchanged,
// so an existing `ff-palette` value is adopted as-is; see lib/durable-json.ts.

import { readDurable, writeDurable } from "@/lib/durable-json";

const STORAGE_KEY = "ff-palette";
const MAX_RECENT = 6;

function parseRecent(raw: unknown): string[] {
  const recent = (raw as { recent?: unknown } | null)?.recent;
  if (!Array.isArray(recent)) return [];
  return recent.filter((id): id is string => typeof id === "string");
}

// ── Store ────────────────────────────────────────────────────────────────────

interface PaletteState {
  open: boolean;
  /** Recently run command ids, most-recent first (deduped, capped). */
  recent: string[];
  /** False until the (always-async) durable read has landed. Nothing gates
   *  render on this — recents are only read once ⌘K opens the palette, long
   *  after mount — but a write before it flips would persist an empty list over
   *  what's on disk, so `pushRecent` waits for it. Runtime-only. */
  hasHydrated: boolean;

  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;
  /** Record that a command ran; bumps it to the front of `recent`. */
  pushRecent: (id: string) => void;
  /** Adopt the persisted recents. Fired once on module load; exported on the
   *  store so tests can re-run it after seeding storage. */
  hydrate: () => Promise<void>;
}

export const usePaletteStore = create<PaletteState>((set, get) => ({
  open: false,
  recent: [],
  hasHydrated: false,

  openPalette: () => set({ open: true }),
  closePalette: () => set({ open: false }),
  togglePalette: () => set((s) => ({ open: !s.open })),

  pushRecent: (id) => {
    const recent = [id, ...get().recent.filter((r) => r !== id)].slice(
      0,
      MAX_RECENT,
    );
    set({ recent });
    if (get().hasHydrated) writeDurable(STORAGE_KEY, { recent });
  },

  hydrate: async () => {
    const stored = await readDurable(STORAGE_KEY, parseRecent, []);
    // Anything pushed while the read was in flight is newer than what was on
    // disk, so it wins — merge rather than clobber.
    set((s) => ({
      recent: [
        ...s.recent,
        ...stored.filter((id) => !s.recent.includes(id)),
      ].slice(0, MAX_RECENT),
      hasHydrated: true,
    }));
  },
}));

void usePaletteStore.getState().hydrate();
