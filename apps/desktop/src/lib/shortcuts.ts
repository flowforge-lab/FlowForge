// Single source of truth for the keyboard-shortcuts cheatsheet (Issue #20). The
// help overlay renders straight off this list, so a future shortcut (command
// palette, step-group toggles, …) appears by adding one entry here — no other
// change. Kept React-free so it's unit-testable.

export type ShortcutGroup = "Composer" | "Navigation" | "Session";

export interface Shortcut {
  /** Display tokens, each rendered as its own <kbd>. "Mod" resolves to ⌘ / Ctrl
   *  at render time (kept out of the data so this module stays platform-pure). */
  keys: string[];
  label: string;
  group: ShortcutGroup;
}

// The order groups appear in the overlay.
const GROUP_ORDER: ShortcutGroup[] = ["Composer", "Navigation", "Session"];

// Seeded with the shortcuts that exist today (input-bar.tsx + app-shell.tsx's
// useGlobalShortcuts). This documents behavior — it does not add any.
export const SHORTCUTS: Shortcut[] = [
  { group: "Composer", keys: ["Enter"], label: "Send message" },
  { group: "Composer", keys: ["Shift", "Enter"], label: "New line" },

  { group: "Navigation", keys: ["Mod", "K"], label: "Command palette" },
  {
    group: "Navigation",
    // Both openers — "or" renders as a plain separator, not a key.
    keys: ["?", "or", "Mod", "/"],
    label: "Keyboard shortcuts (this list)",
  },
  {
    group: "Navigation",
    keys: ["Esc"],
    label: "Close panel / overlay, or stop the turn",
  },

  { group: "Session", keys: ["Mod", "N"], label: "New session" },
  { group: "Session", keys: ["Mod", "1–9"], label: "Jump to session" },
];

/**
 * Group shortcuts for display: groups in GROUP_ORDER, empty groups omitted,
 * each group keeping the registry's order. Drives the overlay, so adding a
 * Shortcut to SHORTCUTS makes it appear with no other change.
 */
export function groupedShortcuts(
  shortcuts: Shortcut[] = SHORTCUTS,
): { group: ShortcutGroup; items: Shortcut[] }[] {
  return GROUP_ORDER.map((group) => ({
    group,
    items: shortcuts.filter((s) => s.group === group),
  })).filter((g) => g.items.length > 0);
}
