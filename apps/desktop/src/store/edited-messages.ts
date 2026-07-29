// Which user messages the user has edited (#929 part B). FE-only and persisted
// under `"ff-edited-messages"` via `durableStorage` (#1121); no IPC, no
// `Message.edited` field.
//
// This works because `edit_user_message` UPDATEs the message row in place and hands
// back the SAME id, so a marker keyed by message id survives both the re-run and a
// relaunch (`loadSession` returns that id again).
//
// Deliberate ceiling: FlowForge truncates and re-runs — the pre-edit content and the
// old response are DELETEd — so this is an honest "you changed this" hint, NOT edit
// history. There is nothing behind it to open, which is why `message-header.tsx`
// renders it as static text rather than a button.
//
// Markers are only ever written on a *successful* edit (see `chat.ts::editMessage`),
// so the hint can never lie. The one drift is markers disappearing if the store is
// cleared while the DB survives — the safe direction.
//
// Hydration is async (`durableStorage` always is), so markers can be absent for a
// frame after mount. Not gated: the hint is decorative, it renders inside message
// rows that only appear once a session has loaded over IPC, and the worst case is
// a late-appearing label rather than a wrong one.

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { durableStorage } from "@/lib/durable-storage";

const STORAGE_KEY = "ff-edited-messages";

export interface EditedMessagesState {
  /** Message ids known to have been edited. An array, not a Set, so `persist`
   *  round-trips it through JSON without a custom serializer. */
  editedIds: string[];
  /** Record `id` as edited. Idempotent. */
  markEdited: (id: string) => void;
  isEdited: (id: string) => boolean;
  /** Drop every marker (test/reset seam). */
  clearEdited: () => void;
}

export const useEditedMessagesStore = create<EditedMessagesState>()(
  persist(
    (set, get) => ({
      editedIds: [],

      markEdited: (id) =>
        set((s) =>
          s.editedIds.includes(id) ? s : { editedIds: [...s.editedIds, id] },
        ),

      isEdited: (id) => get().editedIds.includes(id),

      clearEdited: () => set({ editedIds: [] }),
    }),
    { name: STORAGE_KEY, storage: createJSONStorage(() => durableStorage) },
  ),
);
