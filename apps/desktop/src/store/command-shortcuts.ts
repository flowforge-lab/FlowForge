// Message shortcuts for the Skills → Shortcuts sub-tab (SET.5). A shortcut maps a
// `/name` token to a canned message that's sent verbatim — this is NOT system-prompt
// injection and is distinct from the GLOBAL "Keyboard" section's key bindings.
// FE-only and persisted to localStorage under `"ff-command-shortcuts"`; no IPC.

import { create } from "zustand";
import { persist } from "zustand/middleware";

const STORAGE_KEY = "ff-command-shortcuts";

export interface CommandShortcut {
  id: string;
  /** Invocation token, stored without the leading slash (e.g. `ship`). */
  name: string;
  /** The message text sent when `/name` is invoked. */
  message: string;
}

export interface CommandShortcutsState {
  shortcuts: CommandShortcut[];
  /** Add a shortcut. Trims inputs and the leading slash; no-ops on blank
   *  name/message or a duplicate name (case-insensitive). Returns whether it added. */
  addShortcut: (name: string, message: string) => boolean;
  removeShortcut: (id: string) => void;
  resetShortcuts: () => void;
}

/** Strip a single leading slash and surrounding whitespace from a shortcut name. */
export function normalizeShortcutName(name: string): string {
  return name.trim().replace(/^\/+/, "").trim();
}

export const useCommandShortcutsStore = create<CommandShortcutsState>()(
  persist(
    (set, get) => ({
      shortcuts: [],

      addShortcut: (name, message) => {
        const cleanName = normalizeShortcutName(name);
        const cleanMessage = message.trim();
        if (cleanName === "" || cleanMessage === "") return false;
        const exists = get().shortcuts.some(
          (s) => s.name.toLowerCase() === cleanName.toLowerCase(),
        );
        if (exists) return false;
        set((s) => ({
          shortcuts: [
            ...s.shortcuts,
            {
              id: crypto.randomUUID(),
              name: cleanName,
              message: cleanMessage,
            },
          ],
        }));
        return true;
      },

      removeShortcut: (id) =>
        set((s) => ({ shortcuts: s.shortcuts.filter((x) => x.id !== id) })),

      resetShortcuts: () => set({ shortcuts: [] }),
    }),
    { name: STORAGE_KEY },
  ),
);
