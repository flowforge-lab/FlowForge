import { Gauge } from "@/components/ui/icon";
import { Progress } from "@/components/ui/progress";
import { useChatStore } from "@/store/chat";

// Context-usage gauge (#282, follow-up to #244 R6 / PR #272). Surfaces the
// backend's estimated context token count for a session — recorded in
// `contextTokensBySession` from `TurnDoneEvent.tokenCount` (see chat store
// `finishTurn`). Session-scoped (per pane, #148), so each tiling pane shows its
// own session's usage rather than the global active session.
//
// #598 upgrades this to a usage ratio over the shared `Progress` primitive once
// the backend forwards the effective compaction budget (`context_window(model) *
// 0.8`) on `TurnDoneEvent.budgetTokens` — the denominator the loop actually
// compacts against. Until a turn reports a budget it degrades to the count-only
// chip, so nothing breaks before that backend/contract half lands.

// Compact token count for an inline header chip: exact below 1k, one-decimal
// `k`/`M` above. Prefixed with `≈` at the call site to flag it's an estimate.
function formatTokens(n: number): string {
  if (n < 1_000) return String(n);
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function ContextGauge({ sessionId }: { sessionId: string }) {
  const tokens = useChatStore((s) => s.contextTokensBySession[sessionId]);
  const budget = useChatStore((s) => s.contextBudgetBySession[sessionId]);

  // No estimate yet: before the first completed turn, or a Done that carried no
  // count. Treat both `undefined` (no key) and `null` as "no estimate" and keep
  // the header clean by rendering nothing.
  if (tokens == null) return null;

  // Ratio path (#598): only when a usable budget denominator exists. Guard against
  // a missing/zero budget so we never divide by zero — fall back to count-only.
  // `Progress` clamps to 0–100, so an over-budget turn pegs at 100% rather than
  // overflowing (fixing the old flat-24k denominator that pegged large windows).
  const hasRatio = budget != null && budget > 0;
  const pct = hasRatio ? Math.round((tokens / budget) * 100) : null;

  const detail = hasRatio
    ? `Estimated context usage: ${tokens.toLocaleString()} of ${budget.toLocaleString()} tokens (${pct}%)`
    : `Estimated context usage: ${tokens.toLocaleString()} tokens`;

  return (
    <span
      className="flex shrink-0 items-center gap-1.5 text-xs tabular-nums text-muted-foreground"
      title={detail}
      aria-label={detail}
    >
      {hasRatio ? (
        <Progress
          value={pct ?? 0}
          aria-label={`Context usage ${pct}%`}
          className="h-1 w-10"
        />
      ) : (
        <Gauge className="size-3.5" aria-hidden />
      )}
      <span>≈{formatTokens(tokens)}</span>
    </span>
  );
}
