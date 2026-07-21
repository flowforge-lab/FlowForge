// Which user messages the user has edited (#929 part B). FE-only and persisted to
// localStorage under `"ff-edited-messages"`; no IPC, no `Message.edited` field.
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
// so the hint can never lie. The one drift is markers disappearing if localStorage is
// cleared while the DB survives — the safe direction.

import { create } from "zustand";
import { persist } from "zustand/middleware";

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
    { name: STORAGE_KEY },
  ),
);
