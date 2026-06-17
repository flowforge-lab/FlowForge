import { describe, expect, it } from "vitest";

import { keyboardReferenceGroups } from "@/lib/keyboard-reference";
import { SHORTCUTS, groupedShortcuts } from "@/lib/shortcuts";

describe("keyboardReferenceGroups", () => {
  it("derives groups from the registry, not a duplicated list", () => {
    const groups = keyboardReferenceGroups("enter");
    expect(groups.map((g) => g.group)).toEqual(["General", "Navigation"]);

    // Every rendered item traces back to a registry entry (by label).
    const registryLabels = new Set(SHORTCUTS.map((s) => s.label));
    for (const g of groups) {
      for (const item of g.items) {
        expect(registryLabels.has(item.label)).toBe(true);
      }
    }

    // General == registry Composer + Session; Navigation == registry Navigation.
    const grouped = groupedShortcuts();
    const itemsOf = (name: string) =>
      grouped.find((x) => x.group === name)?.items ?? [];
    const general = groups.find((g) => g.group === "General")!;
    expect(general.items.map((i) => i.label)).toEqual(
      [...itemsOf("Composer"), ...itemsOf("Session")].map((i) => i.label),
    );
    expect(groups.find((g) => g.group === "Navigation")!.items).toEqual(
      itemsOf("Navigation"),
    );
  });

  it("reflects the send-message binding in the Send message / New line rows", () => {
    const enterRows = keyboardReferenceGroups("enter")[0].items;
    expect(enterRows.find((i) => i.label === "Send message")!.keys).toEqual([
      "Enter",
    ]);
    expect(enterRows.find((i) => i.label === "New line")!.keys).toEqual([
      "Shift",
      "Enter",
    ]);

    const ctrlRows = keyboardReferenceGroups("ctrlEnter")[0].items;
    expect(ctrlRows.find((i) => i.label === "Send message")!.keys).toEqual([
      "Mod",
      "Enter",
    ]);
    expect(ctrlRows.find((i) => i.label === "New line")!.keys).toEqual([
      "Enter",
    ]);
  });
});
