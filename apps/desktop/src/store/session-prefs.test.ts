// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";

import { arrangeSessions } from "@/lib/sessions";
import { useSessionPrefsStore } from "@/store/session-prefs";
import type { Session } from "@/bindings";

describe("useSessionPrefsStore", () => {
  beforeEach(() => {
    useSessionPrefsStore.setState({ pinned: [], dismissed: [] });
  });

  it("togglePin adds then removes an id", () => {
    const { togglePin } = useSessionPrefsStore.getState();
    togglePin("a");
    expect(useSessionPrefsStore.getState().pinned).toEqual(["a"]);
    expect(useSessionPrefsStore.getState().isPinned("a")).toBe(true);

    togglePin("a");
    expect(useSessionPrefsStore.getState().pinned).toEqual([]);
    expect(useSessionPrefsStore.getState().isPinned("a")).toBe(false);
  });

  it("dismiss hides an id and restore brings it back (lossless, deduped)", () => {
    const { dismiss, restore } = useSessionPrefsStore.getState();
    dismiss("a");
    dismiss("a"); // deduped
    expect(useSessionPrefsStore.getState().dismissed).toEqual(["a"]);
    expect(useSessionPrefsStore.getState().isDismissed("a")).toBe(true);

    restore("a");
    expect(useSessionPrefsStore.getState().dismissed).toEqual([]);
    expect(useSessionPrefsStore.getState().isDismissed("a")).toBe(false);
  });

  it("dismissing a pinned session also drops the pin", () => {
    const s = useSessionPrefsStore.getState();
    s.togglePin("a");
    s.dismiss("a");
    expect(useSessionPrefsStore.getState().pinned).toEqual([]);
    expect(useSessionPrefsStore.getState().dismissed).toEqual(["a"]);
  });

  it("purge drops a session id from both pinned and dismissed", () => {
    const s = useSessionPrefsStore.getState();
    s.togglePin("a");
    s.dismiss("b");
    s.togglePin("c");
    useSessionPrefsStore.getState().purge("a");
    useSessionPrefsStore.getState().purge("b");
    expect(useSessionPrefsStore.getState().pinned).toEqual(["c"]);
    expect(useSessionPrefsStore.getState().dismissed).toEqual([]);
  });

  // The sidebar feeds this store's `pinned` array straight to `arrangeSessions`,
  // so the append-on-pin contract is what makes a newly pinned session land on
  // top (#1110). Asserted end-to-end here rather than left to a comment.
  it("feeds arrangeSessions a pin order that puts the newest pin on top", () => {
    const mk = (id: string): Session => ({
      id,
      goal: null,
      title: null,
      summary: null,
      status: "active",
      createdAt: 0,
      updatedAt: 0,
    });
    // Backend recency order.
    const sessions = [mk("a"), mk("b"), mk("c")];
    const order = () => useSessionPrefsStore.getState().pinned;
    const ids = () =>
      arrangeSessions(sessions, order(), new Set<string>()).map((s) => s.id);

    // Pin the newest session first — nothing surprising yet.
    useSessionPrefsStore.getState().togglePin("a");
    expect(ids()).toEqual(["a", "b", "c"]);

    // Now pin the OLDEST. Ordering the pinned group by recency would drop it
    // below `a`; it has to land on top instead.
    useSessionPrefsStore.getState().togglePin("c");
    expect(ids()).toEqual(["c", "a", "b"]);

    // Unpin it: `a` stays pinned, `c` rejoins the live group by recency.
    useSessionPrefsStore.getState().togglePin("c");
    expect(ids()).toEqual(["a", "b", "c"]);
  });

  it("persists to localStorage under ff-session-prefs", () => {
    useSessionPrefsStore.getState().togglePin("a");
    useSessionPrefsStore.getState().dismiss("b");
    const blob = JSON.parse(localStorage.getItem("ff-session-prefs") ?? "{}");
    expect(blob.state.pinned).toEqual(["a"]);
    expect(blob.state.dismissed).toEqual(["b"]);
  });

  // #1110 follow-up: `durableStorage` is always async (it replaced plain
  // localStorage, which hydrated synchronously), so the sidebar needs a
  // reliable "has the real read landed yet" signal — painting against the
  // default `pinned: []` before it does is the exact "pin looked like it did
  // nothing" symptom the original fix addressed, just at app-launch time.
  it("hasHydrated eventually flips true once the async storage read resolves", async () => {
    vi.resetModules();
    localStorage.setItem(
      "ff-session-prefs",
      JSON.stringify({ state: { pinned: ["x"], dismissed: [] }, version: 0 }),
    );

    const mod = await import("@/store/session-prefs");
    // A macrotask flush guarantees every pending microtask (the storage read
    // and the persist middleware's chained `.then()`s) has settled, without
    // asserting on an exact tick count.
    await new Promise((r) => setTimeout(r, 0));

    expect(mod.useSessionPrefsStore.getState().hasHydrated).toBe(true);
    expect(mod.useSessionPrefsStore.getState().pinned).toEqual(["x"]);
  });

  it("excludes hasHydrated from what gets written to storage (partialize)", () => {
    // Runtime-only signal, not a real preference — a previous session's
    // `true` must never leak in as a NEW session's starting value, so it
    // can't be allowed onto disk in the first place.
    useSessionPrefsStore.setState({ hasHydrated: true });
    useSessionPrefsStore.getState().togglePin("a");

    const blob = JSON.parse(localStorage.getItem("ff-session-prefs") ?? "{}");
    expect(blob.state).not.toHaveProperty("hasHydrated");
    expect(blob.state.pinned).toEqual(["a"]);
  });
});
