// Forced-open bus for the in-thread find bar (#875). The matching set
// (`searchInSession`) covers text inside folded sub-blocks (StepGroup's "N
// steps" header, ToolStepBlock, long OutputBlock). When the find bar needs to
// paint a highlight in one of those sub-blocks, it tells this store the
// collapsible should be opened — *while find is open*. The collapser consults
// its slot here on every render and yields to a user choice when the store is
// empty (find closed → manual toggle wins again, default-folded state
// restored).
//
// Single set scoped to one find bar at a time. The bus is global (Zustand
// store) so deep-collapsed children can subscribe without prop-drilling; the
// find bar clears it on close and on every new search so no cross-session
// leakage. Idempotent: opening the same id twice is a no-op, calling
// `forceOpenMany([])` is a no-op, resetting mid-step is cheap (small string
// set, low cardinality for one search).
//
// Stable ids (see `lib/find-occurrences.ts` for the derivation):
//
//   • `tool-step:<callId>`       — opens the per-step ToolStepBlock (covers its args + folded result)
//   • `step-group:<messageId>:<segKey>` — opens the StepGroup wrapping a steps segment
//   • `output:<messageId>`      — opens an OutputBlock folded because its body exceeded
//                                 `OUTPUT_FOLD_THRESHOLD` (persisted `tool`/`system` rows)
//
// Reasoning text (ThinkingBlock) is opt-out via `data-skip-find` (see
// `find-highlight.ts`), so it never opts in here.

import { create } from "zustand";

interface FindExpansionState {
  /** Stable ids of collapsers the active find bar has demanded open. Empty when no
   *  find is in flight, or after the bar has cleared. Subscribe via
   *  `useFindExpansion((s) => s.has(id))` in a collapser. */
  forced: Set<string>;
  /** Open one or more collapsers — idempotent, preserves existing ids. */
  forceOpenMany: (ids: string[]) => void;
  /** Replace the entire forced set (used between searches). Idempotent for []. */
  setForced: (ids: string[]) => void;
  /** Drop the entire forced set — called on find close so user toggles win again. */
  clear: () => void;
}

export const useFindExpansion = create<FindExpansionState>((set) => ({
  forced: new Set<string>(),
  forceOpenMany: (ids) =>
    set((s) => {
      if (ids.length === 0) return s;
      const next = new Set(s.forced);
      let changed = false;
      for (const id of ids) {
        if (!next.has(id)) {
          next.add(id);
          changed = true;
        }
      }
      return changed ? { forced: next } : s;
    }),
  setForced: (ids) => set(() => ({ forced: new Set(ids) })),
  clear: () => set(() => ({ forced: new Set<string>() })),
}));

/**
 * Subscribe-friendly hook: `true` when the find bar has demanded this collapser
 * open. Returns `false` while no find is in flight. Stable across re-renders as
 * long as the id is unchanged and the set is unchanged.
 */
export function useIsForcedOpen(id: string | undefined): boolean {
  return useFindExpansion((s) => (id ? s.forced.has(id) : false));
}
