// "Open in Split" side-panel state. Pure frontend (Issue #11) — no IPC. The
// panel pops rich content (code blocks, long tool output) into a wide, stable,
// scrollable surface to the right of the chat column.

import { create } from "zustand";

// Discriminated union of what the panel can show. Future kinds ("web", "file",
// "diff") get added here; the `switch` in split-panel.tsx is exhaustive, so a
// new kind without a matching case is a compile-time error (the TODO finds you).
// The workspace file browser used to be a `{ kind: "files" }` variant here, but
// moved to a per-pane surface in #944 (see store/file-panel.ts).
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
//
// Stored through `durableStorage` (#1134) — a WKWebView doesn't reliably flush
// localStorage before the process exits, so a panel opened or resized late in a
// session could otherwise come back closed. The on-disk shape is unchanged, so
// an existing `ff-split` value is adopted as-is; see lib/durable-json.ts.

import { readDurable, writeDurable } from "@/lib/durable-json";

const STORAGE_KEY = "ff-split";

interface Persisted {
  open: boolean;
  width: number;
  wrap: boolean;
  content: SplitContent | null;
}

const FALLBACK: Persisted = {
  open: false,
  width: DEFAULT_WIDTH,
  wrap: true,
  content: null,
};

/**
 * Accept a persisted payload only if it is a `SplitContent` this build still
 * knows how to render; anything else loads as "nothing open".
 *
 * This is not defensive dressing. The blob outlives the code that wrote it: a
 * user who opened the workspace file browser before #944 moved it out of this
 * union still has `{ kind: "files" }` stored, and the `switch` in
 * split-panel.tsx has no case for it. TypeScript cannot help here — the union
 * describes what this build writes, not what a months-old profile holds — so
 * the boundary has to be checked at runtime, once, on the way in.
 *
 * Without it the panel handed that object to React as a child, React threw, and
 * because `SplitPanel` renders above the pane tree the error boundary replaced
 * the whole window. `open` is persisted, so it recurred on every launch.
 */
function parseContent(value: unknown): SplitContent | null {
  if (!value || typeof value !== "object") return null;
  const c = value as { kind?: unknown; lang?: unknown; text?: unknown };
  if (typeof c.text !== "string") return null;
  if (c.kind === "text") return value as SplitContent;
  if (c.kind === "code" && typeof c.lang === "string") {
    return value as SplitContent;
  }
  return null;
}

function parsePersisted(raw: unknown): Persisted {
  const p = (raw ?? {}) as Partial<Persisted>;
  return {
    open: Boolean(p.open),
    width: clampSplitWidth(p.width ?? DEFAULT_WIDTH),
    wrap: p.wrap ?? true,
    content: parseContent(p.content),
  };
}

// ── Store ────────────────────────────────────────────────────────────────────

interface SplitState extends Persisted {
  /** False until the (always-async) durable read has landed. Nothing gates
   *  render on it — `split-panel.tsx` returns null while closed and has no width
   *  transition, so a panel restored open simply paints a tick later rather than
   *  animating in — but a write before it flips would persist a closed panel
   *  over the open one on disk, so `save` waits for it. Runtime-only. */
  hasHydrated: boolean;

  openInSplit: (content: SplitContent) => void;
  closeSplit: () => void;
  /** Flip the panel open/closed without changing its content (⌘K palette). When
   *  re-opened with no prior content it shows the panel's empty state. */
  toggleSplit: () => void;
  setWidth: (px: number) => void;
  toggleWrap: () => void;
  /** Adopt the persisted panel state. Fired once on module load; exported on the
   *  store so tests can re-run it after seeding storage. */
  hydrate: () => Promise<void>;
}

export const useSplitStore = create<SplitState>((set, get) => {
  const save = () => {
    // Before hydration the state is still defaults; writing them would clobber
    // the panel the user actually left open.
    if (!get().hasHydrated) return;
    const { open, width, wrap, content } = get();
    writeDurable(STORAGE_KEY, { open, width, wrap, content });
  };

  return {
    ...FALLBACK,
    hasHydrated: false,

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

    hydrate: async () => {
      const stored = await readDurable(STORAGE_KEY, parsePersisted, FALLBACK);
      // A panel opened while the read was in flight is newer than what's on
      // disk — keep it rather than yanking it shut under the user.
      set((s) =>
        s.open ? { hasHydrated: true } : { ...stored, hasHydrated: true },
      );
    },
  };
});

void useSplitStore.getState().hydrate();
