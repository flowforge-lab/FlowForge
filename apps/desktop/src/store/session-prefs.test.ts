// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";

import { useSessionPrefsStore } from "@/store/session-prefs";

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

  it("persists to localStorage under ff-session-prefs", () => {
    useSessionPrefsStore.getState().togglePin("a");
    useSessionPrefsStore.getState().dismiss("b");
    const blob = JSON.parse(localStorage.getItem("ff-session-prefs") ?? "{}");
    expect(blob.state.pinned).toEqual(["a"]);
    expect(blob.state.dismissed).toEqual(["b"]);
  });
});
