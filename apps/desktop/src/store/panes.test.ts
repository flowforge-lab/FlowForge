import { describe, it, expect, beforeEach } from "vitest";

import {
  usePanesStore,
  leaves,
  clampRatio,
  MIN_RATIO,
  MAX_PANES,
  type PaneNode,
  type SplitNode,
} from "@/store/panes";
import { useChatStore } from "@/store/chat";

// vitest runs in the `node` environment (no DOM); give the store a real
// localStorage so persist -> reload (init reconcile) can be exercised.
class MemoryStorage {
  private m = new Map<string, string>();
  getItem(k: string) {
    return this.m.has(k) ? (this.m.get(k) as string) : null;
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
  clear() {
    this.m.clear();
  }
}

const root = () => usePanesStore.getState().root as PaneNode;
const focused = () => usePanesStore.getState().focusedPaneId;

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  usePanesStore.setState({ root: null, focusedPaneId: null });
  useChatStore.setState({ activeSessionId: null });
});

describe("panes store (#148)", () => {
  it("init seeds a single focused leaf on the active session", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const r = root();
    expect(r.type).toBe("leaf");
    expect(leaves(r)).toHaveLength(1);
    expect(leaves(r)[0].sessionId).toBe("s1");
    expect(focused()).toBe(leaves(r)[0].id);
  });

  it("init with no live sessions leaves an empty tree", () => {
    usePanesStore.getState().init([], null);
    expect(usePanesStore.getState().root).toBeNull();
    expect(usePanesStore.getState().focusedPaneId).toBeNull();
  });

  it("splitRight adds a vertical split with the new leaf focused and active", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const leafId = leaves(root())[0].id;
    usePanesStore.getState().splitRight(leafId, "s2");

    const r = root() as SplitNode;
    expect(r.type).toBe("split");
    expect(r.dir).toBe("vertical");
    expect(r.ratio).toBe(0.5);
    const all = leaves(r);
    expect(all).toHaveLength(2);
    const newLeaf = all.find((l) => l.sessionId === "s2")!;
    expect(focused()).toBe(newLeaf.id);
    expect(useChatStore.getState().activeSessionId).toBe("s2");
  });

  it("splitDown uses a horizontal split", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitDown(leaves(root())[0].id, "s2");
    expect((root() as SplitNode).dir).toBe("horizontal");
  });

  it("does not split beyond MAX_PANES", () => {
    usePanesStore.getState().init(["s1"], "s1");
    for (let i = 2; i <= MAX_PANES; i++) {
      const target = leaves(root())[0].id;
      usePanesStore.getState().splitRight(target, `s${i}`);
    }
    expect(usePanesStore.getState().leafCount()).toBe(MAX_PANES);
    const before = JSON.stringify(root());
    usePanesStore.getState().splitRight(leaves(root())[0].id, "overflow");
    expect(usePanesStore.getState().leafCount()).toBe(MAX_PANES);
    expect(JSON.stringify(root())).toBe(before);
  });

  it("closePane collapses the parent split into the surviving sibling", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const first = leaves(root())[0].id;
    usePanesStore.getState().splitRight(first, "s2");
    const newLeaf = leaves(root()).find((l) => l.sessionId === "s2")!;

    usePanesStore.getState().closePane(newLeaf.id);
    const r = root();
    expect(r.type).toBe("leaf");
    expect(leaves(r)).toHaveLength(1);
    expect(leaves(r)[0].sessionId).toBe("s1");
  });

  it("closePane refocuses a survivor and syncs the active session", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const first = leaves(root())[0].id;
    usePanesStore.getState().splitRight(first, "s2");
    const newLeaf = leaves(root()).find((l) => l.sessionId === "s2")!;

    usePanesStore.getState().closePane(newLeaf.id);
    expect(focused()).toBe(leaves(root())[0].id);
    expect(useChatStore.getState().activeSessionId).toBe("s1");
  });

  it("closePane is a no-op on the last pane", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const only = leaves(root())[0].id;
    usePanesStore.getState().closePane(only);
    expect(usePanesStore.getState().leafCount()).toBe(1);
    expect(root().type).toBe("leaf");
  });

  it("setRatio clamps to [MIN_RATIO, 1-MIN_RATIO]", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    const splitId = (root() as SplitNode).id;

    usePanesStore.getState().setRatio(splitId, 0.99);
    expect((root() as SplitNode).ratio).toBeCloseTo(1 - MIN_RATIO);
    usePanesStore.getState().setRatio(splitId, -1);
    expect((root() as SplitNode).ratio).toBeCloseTo(MIN_RATIO);
    usePanesStore.getState().setRatio(splitId, 0.4);
    expect((root() as SplitNode).ratio).toBeCloseTo(0.4);
  });

  it("focusPane sets focus and syncs the active session", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const first = leaves(root())[0].id;
    usePanesStore.getState().splitRight(first, "s2");
    useChatStore.setState({ activeSessionId: "s2" });

    usePanesStore.getState().focusPane(first);
    expect(focused()).toBe(first);
    expect(useChatStore.getState().activeSessionId).toBe("s1");
  });

  it("setPaneSession swaps a leaf's session and focuses it", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const first = leaves(root())[0].id;
    usePanesStore.getState().setPaneSession(first, "s9");
    expect(leaves(root())[0].sessionId).toBe("s9");
    expect(focused()).toBe(first);
    expect(useChatStore.getState().activeSessionId).toBe("s9");
  });

  it("toggleCollapse flips the leaf's collapsed flag", () => {
    usePanesStore.getState().init(["s1"], "s1");
    const first = leaves(root())[0].id;
    usePanesStore.getState().toggleCollapse(first);
    expect(leaves(root())[0].collapsed).toBe(true);
    usePanesStore.getState().toggleCollapse(first);
    expect(leaves(root())[0].collapsed).toBe(false);
  });

  it("init drops persisted leaves whose session no longer exists", () => {
    // Build + persist a 2-pane layout, then reload with s2 gone.
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    expect(usePanesStore.getState().leafCount()).toBe(2);

    usePanesStore.setState({ root: null, focusedPaneId: null });
    usePanesStore.getState().init(["s1"], "s1");

    const r = root();
    expect(leaves(r)).toHaveLength(1);
    expect(leaves(r)[0].sessionId).toBe("s1");
    expect(focused()).toBe(leaves(r)[0].id);
  });

  it("init syncs the chat active session to the restored focused pane", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    // s2's pane is focused after the split; persisted to the memory storage.

    usePanesStore.setState({ root: null, focusedPaneId: null });
    useChatStore.setState({ activeSessionId: "unrelated" });
    usePanesStore.getState().init(["s1", "s2"], "s1");

    const focusedLeaf = leaves(root()).find((l) => l.id === focused())!;
    expect(useChatStore.getState().activeSessionId).toBe(focusedLeaf.sessionId);
  });

  it("clampRatio guards against non-finite input", () => {
    expect(clampRatio(Number.NaN)).toBe(0.5);
    expect(clampRatio(2)).toBeCloseTo(1 - MIN_RATIO);
  });
});
