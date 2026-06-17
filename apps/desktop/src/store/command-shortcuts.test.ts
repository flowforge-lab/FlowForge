import { beforeEach, describe, expect, it } from "vitest";

import {
  normalizeShortcutName,
  useCommandShortcutsStore,
} from "@/store/command-shortcuts";

describe("normalizeShortcutName", () => {
  it("strips a leading slash and trims whitespace", () => {
    expect(normalizeShortcutName("  /ship ")).toBe("ship");
    expect(normalizeShortcutName("plain")).toBe("plain");
    expect(normalizeShortcutName("//x")).toBe("x");
  });
});

describe("useCommandShortcutsStore", () => {
  beforeEach(() => {
    useCommandShortcutsStore.setState({ shortcuts: [] });
  });

  it("adds a shortcut, normalizing the name", () => {
    const added = useCommandShortcutsStore
      .getState()
      .addShortcut("/ship", "Open a PR");
    expect(added).toBe(true);
    const [s] = useCommandShortcutsStore.getState().shortcuts;
    expect(s.name).toBe("ship");
    expect(s.message).toBe("Open a PR");
    expect(s.id).toBeTruthy();
  });

  it("rejects a blank name or message", () => {
    const { addShortcut } = useCommandShortcutsStore.getState();
    expect(addShortcut("  ", "msg")).toBe(false);
    expect(addShortcut("name", "   ")).toBe(false);
    expect(useCommandShortcutsStore.getState().shortcuts).toHaveLength(0);
  });

  it("rejects a duplicate name case-insensitively", () => {
    const { addShortcut } = useCommandShortcutsStore.getState();
    expect(addShortcut("ship", "first")).toBe(true);
    expect(addShortcut("/SHIP", "second")).toBe(false);
    expect(useCommandShortcutsStore.getState().shortcuts).toHaveLength(1);
  });

  it("removes by id and resets all", () => {
    const { addShortcut } = useCommandShortcutsStore.getState();
    addShortcut("a", "one");
    addShortcut("b", "two");
    const id = useCommandShortcutsStore.getState().shortcuts[0].id;

    useCommandShortcutsStore.getState().removeShortcut(id);
    expect(useCommandShortcutsStore.getState().shortcuts).toHaveLength(1);

    useCommandShortcutsStore.getState().resetShortcuts();
    expect(useCommandShortcutsStore.getState().shortcuts).toEqual([]);
  });
});
