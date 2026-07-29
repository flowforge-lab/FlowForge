// Message shortcuts for the Skills → Shortcuts sub-tab (SET.5). A shortcut maps a
// `/name` token to a canned message that's sent verbatim — this is NOT system-prompt
// injection and is distinct from the GLOBAL "Keyboard" section's key bindings.
// FE-only and persisted under `"ff-command-shortcuts"` via `durableStorage`
// (#1121); no IPC.
//
// Hydration is async (`durableStorage` always is). Not gated on it: the list is
// read when the composer resolves a typed `/name`, which is many frames after
// mount, and an unhydrated store just means the token isn't recognised yet.

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { durableStorage } from "@/lib/durable-storage";

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
    { name: STORAGE_KEY, storage: createJSONStorage(() => durableStorage) },
  ),
);
