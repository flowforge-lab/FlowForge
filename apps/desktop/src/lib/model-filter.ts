// Model-picker filtering (#1301). Kept out of the component so the ranking is
// unit-testable without a DOM, mirroring how `palette.ts` holds the command
// palette's matcher.
//
// Scoped to one provider at a time: the picker keeps its connection → models
// shape, and each connection's submenu filters only its own catalog. So this
// takes a plain list of model ids and returns the same.
//
// Matching reuses the palette's `fuzzyScore` rather than a second scorer: model
// ids are `vendor/model:variant`, so a user who types `gpt5` means
// `openai/gpt-5.6` and `son5` means `anthropic/claude-sonnet-5` — a subsequence
// match with a word-boundary bonus is exactly right, and `/`, `-`, and `_` are
// already word boundaries there.

import { fuzzyScore } from "@/lib/palette";

/** Below this many models, a provider's submenu shows no filter box: it would
 *  add a step for no gain (a local runtime lists a handful). Exported so the
 *  component and its tests agree on the threshold. */
export const FILTER_THRESHOLD = 8;

/**
 * Rank one provider's `models` against `query`.
 *
 * An empty query keeps the provider's own order — the list the user already
 * knows. Otherwise: score descending, then alphabetical, so equally-good
 * matches have a stable order instead of whatever the provider happened to
 * return.
 */
export function filterModels(query: string, models: string[]): string[] {
  const trimmed = query.trim();
  if (!trimmed) return models;

  return models
    .map((model) => ({ model, score: fuzzyScore(trimmed, model) }))
    .filter((x): x is { model: string; score: number } => x.score !== null)
    .sort((a, b) => b.score - a.score || a.model.localeCompare(b.model))
    .map((x) => x.model);
}
