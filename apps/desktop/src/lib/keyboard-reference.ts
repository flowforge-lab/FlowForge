// Re-presents the single keyboard-shortcut registry (lib/shortcuts.ts) for the
// Settings → Keyboard section (SET.6): registry `Composer` + `Session` become
// **General**, `Navigation` stays **Navigation**, and the `Send message` /
// `New line` rows reflect the live `sendMessageKey` so the cheatsheet stays
// truthful. React-free so it's unit-testable and proves the section derives from
// the registry rather than a duplicated copy.

import { groupedShortcuts, SHORTCUTS, type Shortcut } from "@/lib/shortcuts";
import type { SendMessageKey } from "@/store/prefs";

/** A display group for the reference (label + items, all sourced from the registry). */
export interface KeyboardRefGroup {
  group: string;
  items: Shortcut[];
}

export function keyboardReferenceGroups(
  sendMessageKey: SendMessageKey,
  shortcuts: Shortcut[] = SHORTCUTS,
): KeyboardRefGroup[] {
  const grouped = groupedShortcuts(shortcuts);
  const itemsOf = (g: string) =>
    grouped.find((x) => x.group === g)?.items ?? [];

  const reflect = (s: Shortcut): Shortcut => {
    if (s.label === "Send message") {
      return {
        ...s,
        keys: sendMessageKey === "ctrlEnter" ? ["Mod", "Enter"] : ["Enter"],
      };
    }
    if (s.label === "New line") {
      return {
        ...s,
        keys: sendMessageKey === "ctrlEnter" ? ["Enter"] : ["Shift", "Enter"],
      };
    }
    return s;
  };

  return [
    {
      group: "General",
      items: [...itemsOf("Composer"), ...itemsOf("Session")].map(reflect),
    },
    { group: "Navigation", items: itemsOf("Navigation") },
  ].filter((g) => g.items.length > 0);
}
