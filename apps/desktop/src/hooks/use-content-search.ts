// Cross-session full-text search (#710, #876): debounce keystrokes before
// hitting the FTS backend, guard against stale in-flight responses, and track
// whether a search is still resolving for the current query (so callers can
// hold an empty state during the debounce instead of flashing "no results").
// Extracted from session-sidebar.tsx so both the sidebar's compact dropdown
// (#876 surface A) and the full-screen "All Conversations" modal (#876
// surface B) share one implementation.

import { useEffect, useState } from "react";
import { ipc } from "@/lib/ipc";
import type { SearchHit } from "@/bindings/SearchHit";

const DEFAULT_DEBOUNCE_MS = 200;
const DEFAULT_LIMIT = 30;

export interface UseContentSearchOptions {
  limit?: number;
  debounceMs?: number;
}

export interface UseContentSearchResult {
  hits: SearchHit[];
  /** A search is still resolving for the current (trimmed) query. */
  pending: boolean;
}

export function useContentSearch(
  query: string,
  {
    limit = DEFAULT_LIMIT,
    debounceMs = DEFAULT_DEBOUNCE_MS,
  }: UseContentSearchOptions = {},
): UseContentSearchResult {
  const [hits, setHits] = useState<SearchHit[]>([]);
  // The trimmed query `hits` currently reflects — lets us tell "search in
  // flight" from "search returned nothing" so an empty state doesn't flash
  // during the debounce (#747 C2).
  const [searchedFor, setSearchedFor] = useState("");

  useEffect(() => {
    const q = query.trim();
    let cancelled = false;
    // All state writes stay inside the (async) timer so the effect body never
    // sets state synchronously; an emptied query clears with no delay.
    const timer = window.setTimeout(
      async () => {
        if (!q) {
          setHits([]);
          setSearchedFor("");
          return;
        }
        try {
          const results = await ipc.searchMessages(q, limit);
          if (!cancelled) {
            setHits(results);
            setSearchedFor(q);
          }
        } catch {
          if (!cancelled) {
            setHits([]);
            setSearchedFor(q);
          }
        }
      },
      q ? debounceMs : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query, limit, debounceMs]);

  const pending = query.trim().length > 0 && query.trim() !== searchedFor;

  return { hits, pending };
}
