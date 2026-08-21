// Per-session open state for the message navigator (#1290). Small, but it is
// the seam the ⌘⇧O shortcut and the Esc guard in `app-shell.tsx` both act
// through, and split panes are the reason it is keyed at all.

import { afterEach, describe, expect, it } from "vitest";

import { useMessageNavigator } from "@/store/message-navigator";

const reset = () =>
  useMessageNavigator.setState({ openSessions: new Set<string>() });

afterEach(reset);

describe("useMessageNavigator (#1290)", () => {
  it("opens, closes and toggles a session", () => {
    const { openNavigator, closeNavigator, toggleNavigator } =
      useMessageNavigator.getState();
    const open = () => useMessageNavigator.getState().openSessions.has("s1");

    openNavigator("s1");
    expect(open()).toBe(true);

    closeNavigator("s1");
    expect(open()).toBe(false);

    toggleNavigator("s1");
    expect(open()).toBe(true);
    toggleNavigator("s1");
    expect(open()).toBe(false);
  });

  it("keeps split panes independent", () => {
    // ⌘⇧O targets the focused pane; pane B's popup must not open with it.
    useMessageNavigator.getState().openNavigator("s1");

    const { openSessions } = useMessageNavigator.getState();
    expect(openSessions.has("s1")).toBe(true);
    expect(openSessions.has("s2")).toBe(false);
  });

  it("hands back a fresh Set so subscribers re-render", () => {
    // Mutating the Set in place is invisible to zustand, which compares by
    // identity — the popup would open in the store and not on screen.
    const before = useMessageNavigator.getState().openSessions;

    useMessageNavigator.getState().openNavigator("s1");

    expect(useMessageNavigator.getState().openSessions).not.toBe(before);
  });

  it("is idempotent, so a double close is harmless", () => {
    // Escape can arrive twice: once through the app-shell guard and once
    // through radix's own `onEscapeKeyDown`.
    useMessageNavigator.getState().openNavigator("s1");
    const { closeNavigator } = useMessageNavigator.getState();

    closeNavigator("s1");
    const after = useMessageNavigator.getState().openSessions;
    closeNavigator("s1");

    expect(useMessageNavigator.getState().openSessions).toBe(after);
  });
});
