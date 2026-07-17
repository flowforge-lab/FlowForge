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
import { ipc } from "@/lib/ipc";

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
    expect(r.ratios).toEqual([0.5, 0.5]);
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

  it("same-axis splits build one flat N-way row, not a nested tree (#985)", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s3");

    const r = root() as SplitNode;
    expect(r.type).toBe("split");
    // A single split with three flat children — no nesting.
    expect(r.children).toHaveLength(3);
    expect(r.children.every((c) => c.type === "leaf")).toBe(true);
    expect(r.ratios).toHaveLength(3);
    expect(r.ratios.reduce((a, b) => a + b, 0)).toBeCloseTo(1);
  });

  it("a new split appears adjacent to the invoked pane (#985)", () => {
    usePanesStore.getState().init(["s1"], "s1");
    // Build a 3-column row: [s1, s2, s3].
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s3");
    // s1 is leftmost, s3 sits between s1 and s2.
    expect(leaves(root()).map((l) => l.sessionId)).toEqual(["s1", "s3", "s2"]);

    // Split the MIDDLE pane (s3): the new pane lands directly after it.
    const middle = leaves(root()).find((l) => l.sessionId === "s3")!;
    usePanesStore.getState().splitRight(middle.id, "s4");
    expect(leaves(root()).map((l) => l.sessionId)).toEqual([
      "s1",
      "s3",
      "s4",
      "s2",
    ]);
  });

  it("cross-axis split nests inside the row (#985)", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    const first = leaves(root())[0].id;
    // Split the first column DOWN — the other axis — so it nests.
    usePanesStore.getState().splitDown(first, "s3");

    const r = root() as SplitNode;
    expect(r.dir).toBe("vertical");
    expect(r.children).toHaveLength(2);
    const nested = r.children[0] as SplitNode;
    expect(nested.type).toBe("split");
    expect(nested.dir).toBe("horizontal");
    expect(leaves(nested).map((l) => l.sessionId)).toEqual(["s1", "s3"]);
  });

  it("migrates a legacy binary layout and flattens same-dir nesting on load (#985)", () => {
    // Hand-write a pre-#985 left-nested binary tree to localStorage:
    // split(split(P1,P2),P3), all vertical.
    const legacy = {
      root: {
        type: "split",
        id: "outer",
        dir: "vertical",
        ratio: 0.6,
        a: {
          type: "split",
          id: "inner",
          dir: "vertical",
          ratio: 0.5,
          a: { type: "leaf", id: "p1", sessionId: "s1" },
          b: { type: "leaf", id: "p2", sessionId: "s2" },
        },
        b: { type: "leaf", id: "p3", sessionId: "s3" },
      },
      focusedPaneId: "p1",
    };
    localStorage.setItem("ff-panes", JSON.stringify(legacy));

    usePanesStore.getState().init(["s1", "s2", "s3"], "s1");

    const r = root() as SplitNode;
    expect(r.type).toBe("split");
    // Flattened into one vertical row of three leaves.
    expect(r.children).toHaveLength(3);
    expect(r.children.every((c) => c.type === "leaf")).toBe(true);
    expect(leaves(r).map((l) => l.sessionId)).toEqual(["s1", "s2", "s3"]);
    expect(r.ratios).toHaveLength(3);
    expect(r.ratios.reduce((a, b) => a + b, 0)).toBeCloseTo(1);
  });

  it("splitFork clones the pane's session into a new focused pane (#149)", async () => {
    // Use a real mock session so chat.forkSession can clone it server-side.
    const src = await ipc.createSession();
    usePanesStore.getState().init([src.id], src.id);
    const leafId = leaves(root())[0].id;

    await usePanesStore.getState().splitFork(leafId, "vertical");

    const all = leaves(root());
    expect(all).toHaveLength(2);
    const forkedLeaf = all.find((l) => l.sessionId !== src.id)!;
    expect(forkedLeaf.sessionId).not.toBe(src.id);
    expect(focused()).toBe(forkedLeaf.id);
    expect((root() as SplitNode).dir).toBe("vertical");
  });

  it("splitFork is a no-op for an unknown source session", async () => {
    usePanesStore.getState().init(["nope"], "nope");
    await usePanesStore.getState().splitFork(leaves(root())[0].id, "vertical");
    // forkSession rejects the unknown id → store returns null → no split.
    expect(usePanesStore.getState().leafCount()).toBe(1);
  });

  it("splitNew opens a fresh blank session in a focused right split (#245 2a)", async () => {
    const src = await ipc.createSession();
    usePanesStore.getState().init([src.id], src.id);
    const leafId = leaves(root())[0].id;

    await usePanesStore.getState().splitNew(leafId, "vertical");

    const all = leaves(root());
    expect(all).toHaveLength(2);
    const newLeaf = all.find((l) => l.id !== leafId)!;
    // A brand-new session (not the source) lands in the new, focused pane.
    expect(newLeaf.sessionId).not.toBe(src.id);
    expect(focused()).toBe(newLeaf.id);
    expect((root() as SplitNode).dir).toBe("vertical");
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

  it("closePane re-flattens a same-dir split exposed by a collapse (#985 review)", () => {
    // Build: root V:[L1, H:[V:[L2,L4], L3]] — closing L3 collapses the H node
    // into its sole surviving child V:[L2,L4], which then sits directly under
    // the root V split: a same-dir nesting that must be flattened immediately,
    // not left until the next reload.
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2"); // root V:[s1,s2]
    const l2 = leaves(root()).find((l) => l.sessionId === "s2")!;
    usePanesStore.getState().splitDown(l2.id, "s3"); // s2 -> H:[s2,s3]
    const l2Again = leaves(root()).find((l) => l.sessionId === "s2")!;
    usePanesStore.getState().splitRight(l2Again.id, "s4"); // s2 -> V:[s2,s4] (nests inside H)
    const l3 = leaves(root()).find((l) => l.sessionId === "s3")!;

    usePanesStore.getState().closePane(l3.id);

    const r = root() as SplitNode;
    expect(r.type).toBe("split");
    expect(r.dir).toBe("vertical");
    // Flattened: one vertical row of three leaves, no nested V-in-V.
    expect(r.children).toHaveLength(3);
    expect(r.children.every((c) => c.type === "leaf")).toBe(true);
    expect(
      leaves(r)
        .map((l) => l.sessionId)
        .sort(),
    ).toEqual(["s1", "s2", "s4"]);
    expect(r.ratios.reduce((a, b) => a + b, 0)).toBeCloseTo(1);
  });

  it("setRatios normalizes a split's ratios to sum to 1", () => {
    usePanesStore.getState().init(["s1"], "s1");
    usePanesStore.getState().splitRight(leaves(root())[0].id, "s2");
    const splitId = (root() as SplitNode).id;

    usePanesStore.getState().setRatios(splitId, [0.7, 0.3]);
    expect((root() as SplitNode).ratios).toEqual([0.7, 0.3]);

    // Non-normalized input is renormalized to sum to 1.
    usePanesStore.getState().setRatios(splitId, [3, 1]);
    const r = (root() as SplitNode).ratios;
    expect(r[0]).toBeCloseTo(0.75);
    expect(r[1]).toBeCloseTo(0.25);
    expect(r[0] + r[1]).toBeCloseTo(1);

    // Degenerate input falls back to an equal split.
    usePanesStore.getState().setRatios(splitId, [0, 0]);
    expect((root() as SplitNode).ratios).toEqual([0.5, 0.5]);
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
