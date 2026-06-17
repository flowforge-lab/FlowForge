// Tiling pane manager (Issue #148). The main chat area is a recursive binary tree
// of panes; each leaf hosts a full, independent session (its own transcript,
// streaming turn, and composer draft). Splitting a pane replaces that leaf with a
// split node whose two children are the original leaf and a new one. Closing a
// pane removes its leaf and collapses the parent split into the surviving sibling.
//
// One pane is "focused" at a time (focus ring + keyboard/command target). The
// focused pane's session is mirrored to the chat store's activeSessionId so the
// sidebar highlight and ⌘1-9 / ⌘K shortcuts stay coherent with the single-pane UX.
//
// Pure tree state persists to localStorage (mirrors store/split.ts). The chat
// store is the only outward dependency (focus -> setActiveSession bridge).

import { create } from "zustand";
import { useChatStore } from "@/store/chat";

export type SplitDir = "vertical" | "horizontal";

export interface LeafNode {
  type: "leaf";
  id: string;
  sessionId: string;
  /** Folded to just its header (#148 fold button). */
  collapsed?: boolean;
}

export interface SplitNode {
  type: "split";
  id: string;
  /** "vertical" = side-by-side (the divider is a vertical line); "horizontal" =
   *  stacked (the divider is a horizontal line). */
  dir: SplitDir;
  a: PaneNode;
  b: PaneNode;
  /** Fraction of the split's main axis given to child `a`, in [MIN_RATIO, 1-MIN_RATIO]. */
  ratio: number;
}

export type PaneNode = LeafNode | SplitNode;

/** Hard cap on simultaneous panes (#148). */
export const MAX_PANES = 4;
/** A pane never shrinks below this fraction of its split, so a divider can't be
 *  dragged to make a pane unusable. */
export const MIN_RATIO = 0.15;

const STORAGE_KEY = "ff-panes";

const newId = () => crypto.randomUUID();

export function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return 0.5;
  return Math.max(MIN_RATIO, Math.min(1 - MIN_RATIO, r));
}

// ── Pure tree helpers ────────────────────────────────────────────────────────

export function leaves(node: PaneNode): LeafNode[] {
  return node.type === "leaf" ? [node] : [...leaves(node.a), ...leaves(node.b)];
}

export function leafCountOf(node: PaneNode): number {
  return node.type === "leaf" ? 1 : leafCountOf(node.a) + leafCountOf(node.b);
}

function findLeaf(node: PaneNode, paneId: string): LeafNode | undefined {
  if (node.type === "leaf") return node.id === paneId ? node : undefined;
  return findLeaf(node.a, paneId) ?? findLeaf(node.b, paneId);
}

/** Return a new tree with leaf `paneId` replaced by `replacement` subtree. */
function replaceLeaf(
  node: PaneNode,
  paneId: string,
  replacement: PaneNode,
): PaneNode {
  if (node.type === "leaf") return node.id === paneId ? replacement : node;
  return {
    ...node,
    a: replaceLeaf(node.a, paneId, replacement),
    b: replaceLeaf(node.b, paneId, replacement),
  };
}

/** Return a new tree with leaf `paneId` removed (its parent split collapses into
 *  the sibling subtree), or null if `paneId` is the lone root leaf. */
function removeLeaf(node: PaneNode, paneId: string): PaneNode | null {
  if (node.type === "leaf") return node.id === paneId ? null : node;
  const a = removeLeaf(node.a, paneId);
  if (a === null) return node.b; // a was (or contained only) the removed leaf
  const b = removeLeaf(node.b, paneId);
  if (b === null) return node.a;
  return { ...node, a, b };
}

/** Apply `fn` to leaf `paneId`, returning a new tree. */
function mapLeaf(
  node: PaneNode,
  paneId: string,
  fn: (leaf: LeafNode) => LeafNode,
): PaneNode {
  if (node.type === "leaf") return node.id === paneId ? fn(node) : node;
  return {
    ...node,
    a: mapLeaf(node.a, paneId, fn),
    b: mapLeaf(node.b, paneId, fn),
  };
}

function setRatioOf(node: PaneNode, splitId: string, ratio: number): PaneNode {
  if (node.type === "leaf") return node;
  if (node.id === splitId) return { ...node, ratio: clampRatio(ratio) };
  return {
    ...node,
    a: setRatioOf(node.a, splitId, ratio),
    b: setRatioOf(node.b, splitId, ratio),
  };
}

// ── Persistence ──────────────────────────────────────────────────────────────

interface Persisted {
  root: PaneNode | null;
  focusedPaneId: string | null;
}

function isLeaf(v: unknown): v is LeafNode {
  return (
    typeof v === "object" &&
    v !== null &&
    (v as { type?: unknown }).type === "leaf" &&
    typeof (v as LeafNode).id === "string" &&
    typeof (v as LeafNode).sessionId === "string"
  );
}

function isNode(v: unknown): v is PaneNode {
  if (isLeaf(v)) return true;
  if (
    typeof v === "object" &&
    v !== null &&
    (v as { type?: unknown }).type === "split"
  ) {
    const s = v as SplitNode;
    return (
      typeof s.id === "string" &&
      (s.dir === "vertical" || s.dir === "horizontal") &&
      isNode(s.a) &&
      isNode(s.b)
    );
  }
  return false;
}

function loadPersisted(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { root: null, focusedPaneId: null };
    const p = JSON.parse(raw) as Partial<Persisted>;
    const root = isNode(p.root) ? p.root : null;
    return {
      root,
      focusedPaneId:
        typeof p.focusedPaneId === "string" ? p.focusedPaneId : null,
    };
  } catch {
    return { root: null, focusedPaneId: null };
  }
}

function persist(p: Persisted): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    // Quota / private mode — non-fatal; layout just won't survive reload.
  }
}

/** Drop leaves whose session no longer exists, collapsing their parent splits.
 *  Returns null if nothing survives. */
function reconcile(node: PaneNode | null, live: Set<string>): PaneNode | null {
  if (!node) return null;
  let next: PaneNode | null = node;
  for (const leaf of leaves(node)) {
    if (!live.has(leaf.sessionId)) {
      next = next ? removeLeaf(next, leaf.id) : null;
      if (!next) return null;
    }
  }
  return next;
}

// ── Store ────────────────────────────────────────────────────────────────────

interface PanesState {
  root: PaneNode | null;
  focusedPaneId: string | null;

  /** Build/repair the tree from live sessions. Drops dangling leaves; if nothing
   *  survives, starts a single leaf on `activeSessionId`. */
  init: (liveSessionIds: string[], activeSessionId: string | null) => void;
  leafCount: () => number;
  /** Split `paneId` side-by-side, putting `sessionId` in the new (focused) leaf. */
  splitRight: (paneId: string, sessionId: string) => void;
  /** Split `paneId` stacked, putting `sessionId` in the new (focused) leaf. */
  splitDown: (paneId: string, sessionId: string) => void;
  /** Create a blank session and split `paneId` in `dir`, focusing the new pane. */
  splitNew: (paneId: string, dir: SplitDir) => Promise<void>;
  /** Remove a pane; no-op on the last one. Refocuses a surviving leaf. */
  closePane: (paneId: string) => void;
  setRatio: (splitId: string, ratio: number) => void;
  focusPane: (paneId: string) => void;
  setPaneSession: (paneId: string, sessionId: string) => void;
  toggleCollapse: (paneId: string) => void;
}

export const usePanesStore = create<PanesState>((set, get) => {
  const save = () => {
    const { root, focusedPaneId } = get();
    persist({ root, focusedPaneId });
  };

  // Mirror the focused pane's session into the chat store so the sidebar
  // highlight and keyboard shortcuts track the focused pane.
  const syncActive = (sessionId: string) => {
    useChatStore.getState().setActiveSession(sessionId);
  };

  const split = (paneId: string, sessionId: string, dir: SplitDir) => {
    const { root } = get();
    if (!root || leafCountOf(root) >= MAX_PANES) return;
    const leaf = findLeaf(root, paneId);
    if (!leaf) return;
    const newLeaf: LeafNode = { type: "leaf", id: newId(), sessionId };
    const splitNode: SplitNode = {
      type: "split",
      id: newId(),
      dir,
      a: { ...leaf, collapsed: false },
      b: newLeaf,
      ratio: 0.5,
    };
    set({
      root: replaceLeaf(root, paneId, splitNode),
      focusedPaneId: newLeaf.id,
    });
    syncActive(sessionId);
    save();
  };

  return {
    root: null,
    focusedPaneId: null,

    init: (liveSessionIds, activeSessionId) => {
      const live = new Set(liveSessionIds);
      const persisted = loadPersisted();
      let root = reconcile(persisted.root, live);
      if (!root) {
        if (!activeSessionId) {
          set({ root: null, focusedPaneId: null });
          return;
        }
        root = { type: "leaf", id: newId(), sessionId: activeSessionId };
      }
      const allLeaves = leaves(root);
      const ids = new Set(allLeaves.map((l) => l.id));
      const focusedPaneId =
        persisted.focusedPaneId && ids.has(persisted.focusedPaneId)
          ? persisted.focusedPaneId
          : (allLeaves[0]?.id ?? null);
      set({ root, focusedPaneId });
      save();

      // Reload bootstrap: pull every restored pane's transcript (not just the
      // active one) so background panes aren't blank until clicked, and align
      // the chat store's active session with the restored focus ring.
      const chat = useChatStore.getState();
      const seen = new Set<string>();
      for (const leaf of allLeaves) {
        if (seen.has(leaf.sessionId)) continue;
        seen.add(leaf.sessionId);
        void chat.loadSession(leaf.sessionId);
      }
      const focusedLeaf = allLeaves.find((l) => l.id === focusedPaneId);
      if (focusedLeaf) chat.setActiveSession(focusedLeaf.sessionId);
    },

    leafCount: () => {
      const { root } = get();
      return root ? leafCountOf(root) : 0;
    },

    splitRight: (paneId, sessionId) => split(paneId, sessionId, "vertical"),
    splitDown: (paneId, sessionId) => split(paneId, sessionId, "horizontal"),

    splitNew: async (paneId, dir) => {
      if (get().leafCount() >= MAX_PANES) return;
      await useChatStore.getState().newSession();
      const sessionId = useChatStore.getState().activeSessionId;
      if (!sessionId) return;
      split(paneId, sessionId, dir);
    },

    closePane: (paneId) => {
      const { root } = get();
      if (!root || leafCountOf(root) <= 1) return;
      const next = removeLeaf(root, paneId);
      if (!next) return;
      const survivors = leaves(next);
      const focusedPaneId =
        get().focusedPaneId === paneId ||
        !survivors.some((l) => l.id === get().focusedPaneId)
          ? (survivors[0]?.id ?? null)
          : get().focusedPaneId;
      set({ root: next, focusedPaneId });
      const focused = survivors.find((l) => l.id === focusedPaneId);
      if (focused) syncActive(focused.sessionId);
      save();
    },

    setRatio: (splitId, ratio) => {
      const { root } = get();
      if (!root) return;
      set({ root: setRatioOf(root, splitId, ratio) });
      save();
    },

    focusPane: (paneId) => {
      const { root } = get();
      if (!root) return;
      const leaf = findLeaf(root, paneId);
      if (!leaf) return;
      set({ focusedPaneId: paneId });
      syncActive(leaf.sessionId);
      save();
    },

    setPaneSession: (paneId, sessionId) => {
      const { root } = get();
      if (!root) return;
      set({
        root: mapLeaf(root, paneId, (l) => ({ ...l, sessionId })),
        focusedPaneId: paneId,
      });
      syncActive(sessionId);
      save();
    },

    toggleCollapse: (paneId) => {
      const { root } = get();
      if (!root) return;
      set({
        root: mapLeaf(root, paneId, (l) => ({ ...l, collapsed: !l.collapsed })),
      });
      save();
    },
  };
});
