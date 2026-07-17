// Tiling pane manager (Issue #148). The main chat area is a recursive tree of
// panes; each leaf hosts a full, independent session (its own transcript,
// streaming turn, and composer draft). A split node lays out an ordered list of
// children along one axis (#985): splitting a pane along the same axis as its
// parent inserts an adjacent sibling into that flat row, so N side-by-side session
// columns are one split with N children and N-1 dividers. Splitting along the other
// axis nests a new split. Closing a pane removes its leaf and, when a split falls to
// a single child, collapses that split into the survivor.
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
  /** "vertical" = side-by-side (dividers are vertical lines); "horizontal" =
   *  stacked (dividers are horizontal lines). */
  dir: SplitDir;
  /** Ordered children laid out along the split's main axis (length >= 2). */
  children: PaneNode[];
  /** Fraction of the split's main axis for each child, in [MIN_RATIO, 1-MIN_RATIO],
   *  aligned with `children` and summing to 1. */
  ratios: number[];
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

/** Coerce a ratios array to `count` finite, positive fractions summing to 1.
 *  Falls back to an equal split when the input is missing or degenerate. */
function normalizeRatios(
  ratios: number[] | undefined,
  count: number,
): number[] {
  const equal = Array.from({ length: count }, () => 1 / count);
  if (!ratios || ratios.length !== count) return equal;
  const clean = ratios.map((r) => (Number.isFinite(r) && r > 0 ? r : 0));
  const sum = clean.reduce((acc, r) => acc + r, 0);
  if (sum <= 0) return equal;
  return clean.map((r) => r / sum);
}

// ── Pure tree helpers ────────────────────────────────────────────────────────

export function leaves(node: PaneNode): LeafNode[] {
  return node.type === "leaf" ? [node] : node.children.flatMap(leaves);
}

export function leafCountOf(node: PaneNode): number {
  return node.type === "leaf"
    ? 1
    : node.children.reduce((n, c) => n + leafCountOf(c), 0);
}

function findLeaf(node: PaneNode, paneId: string): LeafNode | undefined {
  if (node.type === "leaf") return node.id === paneId ? node : undefined;
  for (const child of node.children) {
    const hit = findLeaf(child, paneId);
    if (hit) return hit;
  }
  return undefined;
}

/** Return the split that directly contains leaf `paneId`, and that child's index,
 *  or null if the leaf is the root or unknown. */
function findParent(
  node: PaneNode,
  paneId: string,
): { parent: SplitNode; index: number } | null {
  if (node.type === "leaf") return null;
  const index = node.children.findIndex(
    (c) => c.type === "leaf" && c.id === paneId,
  );
  if (index !== -1) return { parent: node, index };
  for (const child of node.children) {
    const hit = findParent(child, paneId);
    if (hit) return hit;
  }
  return null;
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
    children: node.children.map((c) => replaceLeaf(c, paneId, replacement)),
  };
}

/** Return a new tree with leaf `paneId` removed. A split that falls to a single
 *  child collapses into that child; a split's dropped child also drops its ratio
 *  slot (remaining ratios renormalize). Returns null if `paneId` is the root leaf. */
function removeLeaf(node: PaneNode, paneId: string): PaneNode | null {
  if (node.type === "leaf") return node.id === paneId ? null : node;
  const kept: PaneNode[] = [];
  const keptRatios: number[] = [];
  node.children.forEach((child, i) => {
    const next = removeLeaf(child, paneId);
    if (next !== null) {
      kept.push(next);
      keptRatios.push(node.ratios[i]);
    }
  });
  if (kept.length === 0) return null;
  if (kept.length === 1) return kept[0];
  return {
    ...node,
    children: kept,
    ratios: normalizeRatios(keptRatios, kept.length),
  };
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
    children: node.children.map((c) => mapLeaf(c, paneId, fn)),
  };
}

/** Return a new tree with split `splitId` replaced by `updated`. */
function replaceSplit(
  node: PaneNode,
  splitId: string,
  updated: SplitNode,
): PaneNode {
  if (node.type === "leaf") return node;
  if (node.id === splitId) return updated;
  return {
    ...node,
    children: node.children.map((c) => replaceSplit(c, splitId, updated)),
  };
}

function setRatiosOf(
  node: PaneNode,
  splitId: string,
  ratios: number[],
): PaneNode {
  if (node.type === "leaf") return node;
  if (node.id === splitId) {
    return { ...node, ratios: normalizeRatios(ratios, node.children.length) };
  }
  return {
    ...node,
    children: node.children.map((c) => setRatiosOf(c, splitId, ratios)),
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

/** Legacy binary split shape persisted before #985 (`{ a, b, ratio }`). */
interface LegacySplit {
  type: "split";
  id: string;
  dir: SplitDir;
  a: PaneNode;
  b: PaneNode;
  ratio?: number;
}

function isSplitObject(
  v: unknown,
): v is { type: "split"; id: unknown; dir: unknown } {
  return (
    typeof v === "object" &&
    v !== null &&
    (v as { type?: unknown }).type === "split"
  );
}

function isNode(v: unknown): v is PaneNode {
  if (isLeaf(v)) return true;
  if (isSplitObject(v)) {
    const s = v as Partial<SplitNode> & Partial<LegacySplit>;
    if (typeof s.id !== "string") return false;
    if (s.dir !== "vertical" && s.dir !== "horizontal") return false;
    // N-way (post-#985) shape.
    if (Array.isArray(s.children)) {
      return s.children.length >= 2 && s.children.every(isNode);
    }
    // Legacy binary shape — still accepted so `normalizeNode` can migrate it.
    return isNode(s.a) && isNode(s.b);
  }
  return false;
}

/** Migrate + flatten a validated node: convert legacy `{ a, b, ratio }` splits to
 *  the N-way shape, and merge a child split into its parent when they share `dir`
 *  (collapsing left-nested chains into one flat row, per #985). */
function normalizeNode(node: PaneNode): PaneNode {
  if (node.type === "leaf") return node;

  const legacy = node as unknown as LegacySplit;
  let children: PaneNode[];
  let ratios: number[];
  if (Array.isArray(node.children)) {
    children = node.children;
    ratios = normalizeRatios(node.ratios, node.children.length);
  } else {
    // Legacy binary split.
    const r = clampRatio(legacy.ratio ?? 0.5);
    children = [legacy.a, legacy.b];
    ratios = [r, 1 - r];
  }

  const flatChildren: PaneNode[] = [];
  const flatRatios: number[] = [];
  children.forEach((child, i) => {
    const norm = normalizeNode(child);
    if (norm.type === "split" && norm.dir === node.dir) {
      // Same-axis child split: splice its children in, scaling their ratios by
      // this child's slot so the row's proportions are preserved.
      norm.children.forEach((gc, j) => {
        flatChildren.push(gc);
        flatRatios.push(ratios[i] * norm.ratios[j]);
      });
    } else {
      flatChildren.push(norm);
      flatRatios.push(ratios[i]);
    }
  });

  return {
    type: "split",
    id: node.id,
    dir: node.dir,
    children: flatChildren,
    ratios: normalizeRatios(flatRatios, flatChildren.length),
  };
}

function loadPersisted(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { root: null, focusedPaneId: null };
    const p = JSON.parse(raw) as Partial<Persisted>;
    const root = isNode(p.root) ? normalizeNode(p.root) : null;
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
  /** Fork `paneId`'s session (clone its transcript) and split in `dir`, focusing
   *  the new (forked) pane. No-op at the pane cap or if the pane is unknown. */
  splitFork: (paneId: string, dir: SplitDir) => Promise<void>;
  /** Remove a pane; no-op on the last one. Refocuses a surviving leaf. */
  closePane: (paneId: string) => void;
  /** Commit a split's full ratios array (one fraction per child, summing to 1). */
  setRatios: (splitId: string, ratios: number[]) => void;
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

    const parent = findParent(root, paneId);
    let nextRoot: PaneNode;
    if (parent && parent.parent.dir === dir) {
      // Same axis as the parent row: insert an adjacent sibling and split the
      // target's ratio slot in half (keeps the row's other proportions intact).
      const { parent: split, index } = parent;
      const children = [...split.children];
      children.splice(index + 1, 0, newLeaf);
      const half = split.ratios[index] / 2;
      const ratios = [...split.ratios];
      ratios.splice(index, 1, half, half);
      const updated: SplitNode = { ...split, children, ratios };
      nextRoot =
        root === split ? updated : replaceSplit(root, split.id, updated);
    } else {
      // Root leaf, or the other axis: wrap the target in a fresh 2-way split.
      const splitNode: SplitNode = {
        type: "split",
        id: newId(),
        dir,
        children: [{ ...leaf, collapsed: false }, newLeaf],
        ratios: [0.5, 0.5],
      };
      nextRoot = replaceLeaf(root, paneId, splitNode);
    }

    set({ root: nextRoot, focusedPaneId: newLeaf.id });
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

    splitFork: async (paneId, dir) => {
      const { root } = get();
      if (!root || leafCountOf(root) >= MAX_PANES) return;
      const leaf = findLeaf(root, paneId);
      if (!leaf) return;
      const forkedId = await useChatStore
        .getState()
        .forkSession(leaf.sessionId);
      if (!forkedId) return;
      split(paneId, forkedId, dir);
    },

    closePane: (paneId) => {
      const { root } = get();
      if (!root || leafCountOf(root) <= 1) return;
      const removed = removeLeaf(root, paneId);
      if (!removed) return;
      // A collapsed single-child split can leave its survivor adjacent to a
      // same-dir grandparent — re-flatten so the "same-axis never nests"
      // invariant holds immediately, not just after the next reload.
      const next = normalizeNode(removed);
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

    setRatios: (splitId, ratios) => {
      const { root } = get();
      if (!root) return;
      set({ root: setRatiosOf(root, splitId, ratios) });
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
