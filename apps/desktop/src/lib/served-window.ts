// Served context window (#602, Option B follow-up to #538). FE-local shape and
// formatting for the window Ollama is actually serving for a model, plus the
// source that produced it.
//
// PROPOSED CONTRACT — pending backend (Tony): these values will ride on
// `ResolvedModel` once the `/api/ps` probe lands and ts-rs regenerates the binding
// (proposed fields: `contextWindow`, `trainedContextWindow`, `contextWindowSource`).
// Until then this is an FE-local type, seeded in the mock / tests and (once the
// fields exist) populated in the session-model store's `load()` from `resolved.*`.
// No `ipc.ts` / `bindings/` edits here — that regen is the backend owner's.

/** How the effective served window was determined, in precedence order. */
export type ServedWindowSource = "explicit" | "served" | "default";

export interface ServedWindow {
  /** Effective served window, in tokens (the denominator the budget is sized from). */
  window: number;
  /** Trained ceiling (`/api/show` context_length), or null when unknown. */
  trained: number | null;
  /** Which input produced `window`: explicit env var > /api/ps served > default. */
  source: ServedWindowSource;
}

// Compact token formatting for a context window: prefers a clean power-of-two "k"
// (131072 -> "128k", 262144 -> "256k"), falls back to decimal-rounded "k"
// (32000 -> "32k"), exact below 1k. Mirrors the compact style of context-gauge's
// `formatTokens` but tuned for the round window sizes models actually advertise.
export function formatContextWindow(n: number): string {
  if (n >= 1024 && n % 1024 === 0) return `${n / 1024}k`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** Human source line for the served window, surfaced so a mis-set server default
 *  is legible instead of a silent under-fill. */
export function servedWindowSourceLabel(source: ServedWindowSource): string {
  switch (source) {
    case "served":
      return "Auto-detected from server";
    case "explicit":
      return "From FLOWFORGE_OLLAMA_NUM_CTX";
    case "default":
      return "Window not detected — using conservative default";
  }
}
