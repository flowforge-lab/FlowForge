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

const STORAGE_KEY = "ff-palette";
const MAX_RECENT = 6;

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const p = JSON.parse(raw) as { recent?: unknown };
    if (!Array.isArray(p.recent)) return [];
    return p.recent.filter((id): id is string => typeof id === "string");
  } catch {
    return [];
  }
}

function persistRecent(recent: string[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ recent }));
  } catch {
    // Quota / private mode — non-fatal; recents just won't survive reload.
  }
}

// ── Store ────────────────────────────────────────────────────────────────────

interface PaletteState {
  open: boolean;
  /** Recently run command ids, most-recent first (deduped, capped). */
  recent: string[];

  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;
  /** Record that a command ran; bumps it to the front of `recent`. */
  pushRecent: (id: string) => void;
}

export const usePaletteStore = create<PaletteState>((set, get) => ({
  open: false,
  recent: loadRecent(),

  openPalette: () => set({ open: true }),
  closePalette: () => set({ open: false }),
  togglePalette: () => set((s) => ({ open: !s.open })),

  pushRecent: (id) => {
    const recent = [id, ...get().recent.filter((r) => r !== id)].slice(
      0,
      MAX_RECENT,
    );
    persistRecent(recent);
    set({ recent });
  },
}));
