// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";
import { useEditedMessagesStore } from "@/store/edited-messages";

const STORAGE_KEY = "ff-edited-messages";

beforeEach(() => {
  useEditedMessagesStore.setState({ editedIds: [] });
  localStorage.clear();
});

describe("edited-messages store (#929 B)", () => {
  it("marks a message id as edited", () => {
    useEditedMessagesStore.getState().markEdited("msg-1");
    expect(useEditedMessagesStore.getState().isEdited("msg-1")).toBe(true);
    expect(useEditedMessagesStore.getState().isEdited("msg-2")).toBe(false);
  });

  it("is idempotent — marking twice keeps one entry", () => {
    useEditedMessagesStore.getState().markEdited("msg-1");
    useEditedMessagesStore.getState().markEdited("msg-1");
    expect(useEditedMessagesStore.getState().editedIds).toEqual(["msg-1"]);
  });

  it("persists to localStorage under ff-edited-messages so it survives a reload", async () => {
    useEditedMessagesStore.getState().markEdited("msg-1");
    // `persist` writes on the next microtask.
    await Promise.resolve();

    const raw = localStorage.getItem(STORAGE_KEY);
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw!).state.editedIds).toContain("msg-1");

    // Simulate a relaunch: wipe in-memory state, then re-hydrate from what a
    // previous run left on disk. (The wipe itself writes through to storage, so
    // restore the captured payload before rehydrating.)
    useEditedMessagesStore.setState({ editedIds: [] });
    localStorage.setItem(STORAGE_KEY, raw!);
    await useEditedMessagesStore.persist.rehydrate();
    expect(useEditedMessagesStore.getState().isEdited("msg-1")).toBe(true);
  });
});
