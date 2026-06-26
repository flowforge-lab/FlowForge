import { Gauge } from "@/components/ui/icon";
import { useChatStore } from "@/store/chat";

// Context-usage gauge (#282, follow-up to #244 R6 / PR #272). Surfaces the
// backend's estimated context token count for a session — recorded in
// `contextTokensBySession` from `TurnDoneEvent.tokenCount` (see chat store
// `finishTurn`). Session-scoped (per pane, #148), so each tiling pane shows its
// own session's usage rather than the global active session.
//
// v1 is count-only by design: the effective denominator (the per-model
// compaction budget, `context_window(model) * 0.8`) is not exposed to the FE
// today, so showing a percentage bar would be misleading. This upgrades to a
// ratio over the shared `Progress` primitive once the backend forwards the
// budget on `TurnDoneEvent` (tracked as a follow-up / contract change).

// Compact token count for an inline header chip: exact below 1k, one-decimal
// `k`/`M` above. Prefixed with `≈` at the call site to flag it's an estimate.
function formatTokens(n: number): string {
  if (n < 1_000) return String(n);
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function ContextGauge({ sessionId }: { sessionId: string }) {
  const tokens = useChatStore((s) => s.contextTokensBySession[sessionId]);

  // No estimate yet: before the first completed turn, or a Done that carried no
  // count. Treat both `undefined` (no key) and `null` as "no estimate" and keep
  // the header clean by rendering nothing.
  if (tokens == null) return null;

  return (
    <span
      className="flex shrink-0 items-center gap-1 text-xs tabular-nums text-muted-foreground"
      title={`Estimated context usage: ${tokens.toLocaleString()} tokens`}
      aria-label={`Estimated context usage: ${tokens.toLocaleString()} tokens`}
    >
      <Gauge className="size-3.5" />
      <span>≈{formatTokens(tokens)}</span>
    </span>
  );
}
