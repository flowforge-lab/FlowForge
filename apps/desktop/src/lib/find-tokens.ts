// Query tokenization for the in-thread find bar (#679/#748). Single source of
// truth so the DOM highlighter (`find-highlight.ts`) and the dev mock
// (`mock.ts`) tokenize identically — and both track the backend FTS5 match set.
//
// The backend (`fts5_escape`, crates/ff-session) wraps each whitespace-separated
// query token in double quotes (`"run" "turn"`), which FTS5 matches as an AND of
// *whole tokens*, any order/distance, case-insensitive, with no prefix (quoting
// disables `*`). We track that by splitting into whole tokens — but on non-word
// characters, not only whitespace, so it isn't full FTS parity (see below).
//
// Known deviations (deliberate, not bugs):
//   • Punctuation-joined multi-word input — the backend splits on WHITESPACE, so
//     `run-turn` stays a single quoted term `"run-turn"` that FTS5 matches as an
//     adjacent phrase (`run` immediately followed by `turn`). We split on non-word
//     chars into independent AND tokens `[run, turn]` matched any-order/any-
//     distance. Net: for such input the FE is *broader* than the backend, never
//     narrower — it can surface a message the backend wouldn't, but never hides a
//     real backend hit. Space-separated queries (the common case) are identical.
//   • Diacritic folding isn't modeled.

// A single FTS5-ish token character: any Unicode letter or digit. Mirrors the
// unicode61 tokenizer's token/separator split closely enough for find (underscore
// and punctuation are separators). Shared with the highlighter's boundary check.
const WORD_CHAR = /[\p{L}\p{N}]/u;
const WORD_RUN = /[\p{L}\p{N}]+/gu;

/**
 * Split `query` into lowercased, de-duplicated whole tokens (runs of Unicode
 * letters/digits). Blank or punctuation-only input yields `[]`. Splitting on
 * non-word chars means hyphen/punctuation-joined terms (`run-turn`) become
 * separate any-distance tokens, not an FTS adjacency phrase — a deliberate
 * simplification (see the module header; the FE stays broader, never narrower).
 */
export function tokenizeQuery(query: string): string[] {
  const matches = query.toLowerCase().match(WORD_RUN);
  if (!matches) return [];
  return [...new Set(matches)];
}

/** True when `ch` is a token character (letter/digit) — used for the highlighter's
 *  whole-token boundary test so it shares one definition with `tokenizeQuery`. */
export function isWordChar(ch: string | undefined): boolean {
  return ch !== undefined && WORD_CHAR.test(ch);
}
