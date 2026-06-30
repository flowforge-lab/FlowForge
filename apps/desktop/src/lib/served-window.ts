// Served context window (#602, Option B follow-up to #538). FE-local readout
// shape over the backend-forwarded `ResolvedModel.{contextWindow,
// trainedContextWindow, contextWindowSource}`. Single-sources `ServedWindowSource`
// as the generated `ContextWindowSource` binding so the FE union can never drift
// from the Rust enum.

import type { ContextWindowSource } from "@/bindings/ContextWindowSource";

/** How the effective served window was determined, in precedence order. Aliases
 *  the generated `ContextWindowSource` binding to keep one source of truth. */
export type ServedWindowSource = ContextWindowSource;

export interface ServedWindow {
  /** Effective served window, in tokens -- the denominator the agent's compaction
   *  budget is sized from (#612). Before the model is loaded the probe falls to the
   *  conservative default, so the chip and the budget agree on a safe under-fill. */
  window: number;
  /** Trained ceiling (`/api/show` context_length), or null when unknown. */
  trained: number | null;
  /** Which input produced `window`: explicit env var > /api/ps served > default. */
  source: ServedWindowSource;
}

// Compact token formatting for a context window. Round-decimal windows render as
// decimal "k" (32000 -> "32k", 128000 -> "128k"); binary windows render as their
// natural power-of-two "k" (131072 -> "128k", 262144 -> "256k"); anything else is
// decimal-rounded, exact below 1k. Decimal is checked first so a decimal-advertised
// 128000 reads "128k" rather than the binary "125k". Mirrors the compact style of
// context-gauge's `formatTokens` but tuned for the round sizes models advertise.
export function formatContextWindow(n: number): string {
  if (n >= 1000 && n % 1000 === 0) return `${n / 1000}k`;
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
    default: {
      const _exhaustive: never = source;
      return _exhaustive;
    }
  }
}
