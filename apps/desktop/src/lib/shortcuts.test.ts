import { describe, it, expect } from "vitest";
import { SHORTCUTS, groupedShortcuts, type Shortcut } from "@/lib/shortcuts";

describe("groupedShortcuts", () => {
  it("includes every registered shortcut exactly once", () => {
    const flattened = groupedShortcuts().flatMap((g) => g.items);
    expect(flattened).toHaveLength(SHORTCUTS.length);
    expect(new Set(flattened)).toEqual(new Set(SHORTCUTS));
  });

  it("orders groups Composer → Navigation → Session", () => {
    expect(groupedShortcuts().map((g) => g.group)).toEqual([
      "Composer",
      "Navigation",
      "Session",
    ]);
  });

  it("omits groups that have no shortcuts", () => {
    const only: Shortcut[] = [
      { group: "Session", keys: ["Mod", "N"], label: "New session" },
    ];
    expect(groupedShortcuts(only).map((g) => g.group)).toEqual(["Session"]);
  });

  it("gives every shortcut its own key combination", () => {
    // The registry is documentation, but a duplicate here means two bindings
    // are fighting over one chord in `useGlobalShortcuts` — which resolves by
    // branch order, silently, and only for whichever one is listed first. This
    // is what guards a new binding (⌘⇧O, #1290) against the next one.
    const combos = SHORTCUTS.map((s) => s.keys.join("+"));

    expect(new Set(combos).size).toBe(combos.length);
  });

  it("lists the message navigator under Navigation (#1290)", () => {
    const nav = groupedShortcuts().find((g) => g.group === "Navigation");

    expect(nav?.items.map((s) => s.label)).toContain("Message navigator");
  });

  it("surfaces a newly added shortcut with no other change", () => {
    const extended: Shortcut[] = [
      ...SHORTCUTS,
      { group: "Navigation", keys: ["Mod", "B"], label: "Toggle sidebar" },
    ];
    const nav = groupedShortcuts(extended).find(
      (g) => g.group === "Navigation",
    );
    expect(nav?.items.map((s) => s.label)).toContain("Toggle sidebar");
  });
});
