// Reveal bus for the virtualized transcript (#1143). A windowed transcript only
// keeps the visible rows in the DOM, so anything that needs to *look at* a
// specific message — the in-thread find bar stepping onto a hit 3000 rows up —
// has to ask the list to mount that row first. Without this, find counts from
// the full data model (`lib/find-occurrences.ts`) but can only paint and scroll
// to rows that happen to be on screen: the counter says "1 of 500" while Enter
// reaches 7 of them.
//
// A store rather than a ref through props: `FindBar` and `ChatView` are siblings
// under `session-pane.tsx`, and the pane has no reason to know that one of them
// can scroll the other. Same shape and reasoning as `store/find-expansion.ts`,
// which is the bus for the other half of this problem (force-opening the
// collapser that hides a match).
//
// Keyed by session id because split panes (#148) each host their own transcript;
// a reveal in pane A must never scroll pane B.

import { create } from "zustand";

/** Mount a message's row and bring it into view. Returns false when the id isn't
 *  in this transcript (a stale occurrence, a session that swapped underneath). */
export type Revealer = (messageId: string) => boolean;

interface TranscriptScrollState {
  revealers: Record<string, Revealer>;
  /** Register this pane's revealer. Returns an unregister for effect cleanup;
   *  only removes the entry if it's still the one registered, so a remount that
   *  re-registers before the old cleanup runs can't blank itself out. */
  register: (sessionId: string, reveal: Revealer) => () => void;
  /** Ask `sessionId`'s transcript to reveal `messageId`. False when no
   *  virtualized transcript is mounted for that session — which is the normal,
   *  correct answer on the non-virtual path, where every row is already in the
   *  DOM and the caller needs to do nothing. */
  reveal: (sessionId: string, messageId: string) => boolean;
}

export const useTranscriptScroll = create<TranscriptScrollState>(
  (set, get) => ({
    revealers: {},

    register: (sessionId, reveal) => {
      set((s) => ({ revealers: { ...s.revealers, [sessionId]: reveal } }));
      return () => {
        set((s) => {
          if (s.revealers[sessionId] !== reveal) return s;
          const { [sessionId]: _gone, ...rest } = s.revealers;
          return { revealers: rest };
        });
      };
    },

    reveal: (sessionId, messageId) =>
      get().revealers[sessionId]?.(messageId) ?? false,
  }),
);
