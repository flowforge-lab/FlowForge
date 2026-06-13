// "Open in Split" side-panel state. Pure frontend (Issue #11) — no IPC. The
// panel pops rich content (code blocks, long tool output) into a wide, stable,
// scrollable surface to the right of the chat column.

import { create } from "zustand";

// Discriminated union of what the panel can show. Future kinds ("web", "file",
// "diff") get added here; the `switch` in split-panel.tsx is exhaustive, so a
// new kind without a matching case is a compile-time error (the TODO finds you).
export type SplitContent =
  | { kind: "code"; lang: string; text: string; title?: string }
  | { kind: "text"; text: string; title?: string };

export const MIN_SPLIT_WIDTH = 320;
export const MAX_SPLIT_WIDTH = 960;
const DEFAULT_WIDTH = 480;

export function clampSplitWidth(px: number): number {
  return Math.max(MIN_SPLIT_WIDTH, Math.min(MAX_SPLIT_WIDTH, Math.round(px)));
}

// ── Persistence ──────────────────────────────────────────────────────────────
// open/width/wrap/content all survive reload so an open panel comes back intact.

const STORAGE_KEY = "ff-split";

interface Persisted {
  open: boolean;
  width: number;
  wrap: boolean;
  content: SplitContent | null;
}

function loadPersisted(): Persisted {
  const fallback: Persisted = {
    open: false,
    width: DEFAULT_WIDTH,
    wrap: true,
    content: null,
  };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return fallback;
    const p = JSON.parse(raw) as Partial<Persisted>;
    return {
      open: Boolean(p.open),
      width: clampSplitWidth(p.width ?? DEFAULT_WIDTH),
      wrap: p.wrap ?? true,
      content: p.content ?? null,
    };
  } catch {
    return fallback;
  }
}

function persist(p: Persisted): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    // Quota / private mode — non-fatal; panel just won't survive reload.
  }
}

// ── Store ────────────────────────────────────────────────────────────────────

interface SplitState extends Persisted {
  openInSplit: (content: SplitContent) => void;
  closeSplit: () => void;
  /** Flip the panel open/closed without changing its content (⌘K palette). When
   *  re-opened with no prior content it shows the panel's empty state. */
  toggleSplit: () => void;
  setWidth: (px: number) => void;
  toggleWrap: () => void;
}

export const useSplitStore = create<SplitState>((set, get) => {
  const save = () => {
    const { open, width, wrap, content } = get();
    persist({ open, width, wrap, content });
  };

  return {
    ...loadPersisted(),

    openInSplit: (content) => {
      set({ open: true, content });
      save();
    },
    closeSplit: () => {
      // Keep `content` so the surface doesn't flash empty mid-close animation;
      // it's simply hidden until the next openInSplit replaces it.
      set({ open: false });
      save();
    },
    toggleSplit: () => {
      set((s) => ({ open: !s.open }));
      save();
    },
    setWidth: (px) => {
      set({ width: clampSplitWidth(px) });
      save();
    },
    toggleWrap: () => {
      set((s) => ({ wrap: !s.wrap }));
      save();
    },
  };
});
