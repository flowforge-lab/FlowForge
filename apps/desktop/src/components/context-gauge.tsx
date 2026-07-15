import { Check, Copy, Gauge } from "@/components/ui/icon";
import { Progress } from "@/components/ui/progress";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { useCopied } from "@/lib/use-copied";
import { useChatStore } from "@/store/chat";
import { useSessionModelStore } from "@/store/session-model";

// Context-usage gauge (#282, follow-up to #244 R6 / PR #272) upgraded into a
// click-to-open "Context Usage" popover (#931). The inline chip surfaces the
// session's context size; opening it reveals a real Bedrock-token breakdown —
// System / Tools / Messages component rows over a segmented bar, the resolved
// model, and cumulative SESSION TOTALS (input/output/cache-read/cache-written).
//
// Data comes from the last completed turn's `TurnDoneEvent` via the chat store:
// `contextInputTokensBySession` (authoritative provider "used", preferred) falls
// back to `contextTokensBySession` (the chars/4 proxy); `contextBudgetBySession`
// is the compaction denominator; `contextBreakdownBySession` drives the bar/rows;
// `sessionTotalsBySession` accumulates usage across turns. Everything but the
// header/pill degrades gracefully — a session with only a `tokenCount` estimate
// (non-Bedrock or a pre-telemetry turn) still renders the chip and header, and
// simply omits the sections it has no data for. Session-scoped (per pane, #148).

// Compact token count matching the reference mock: exact below 1k, else `K`
// (kilo) with a single decimal, trailing `.0` dropped — `173K`, `2.9K`, `5760K`.
// The mock stays in `K` even past a million, so we don't switch to `M`.
function formatK(n: number): string {
  if (n < 1_000) return String(n);
  const k = n / 1_000;
  return `${Number.isInteger(k) ? k : k.toFixed(1)}K`;
}

// Share of the compaction budget as a one-decimal percentage — `7.8%`, `105.6%`.
// Guarded against a missing/zero budget by the caller.
function pctOf(tokens: number, budget: number): string {
  return `${((tokens / budget) * 100).toFixed(1)}%`;
}

// One component row: label · optional count · token figure · %-of-budget.
function ComponentRow({
  swatch,
  label,
  count,
  tokens,
  budget,
}: {
  swatch: string;
  label: string;
  count?: string;
  tokens: number;
  budget: number | undefined;
}) {
  return (
    <div className="flex items-center gap-2 text-xs">
      <span aria-hidden className={`size-2 shrink-0 rounded-sm ${swatch}`} />
      <span className="text-muted-foreground">{label}</span>
      {count ? <span className="text-muted-foreground/70">{count}</span> : null}
      <span className="ml-auto tabular-nums">{formatK(tokens)}</span>
      {budget != null && budget > 0 ? (
        <span className="w-14 text-right tabular-nums text-muted-foreground/70">
          {pctOf(tokens, budget)}
        </span>
      ) : null}
    </div>
  );
}

export function ContextGauge({ sessionId }: { sessionId: string }) {
  const tokens = useChatStore((s) => s.contextTokensBySession[sessionId]);
  const inputTokens = useChatStore(
    (s) => s.contextInputTokensBySession[sessionId],
  );
  const budget = useChatStore((s) => s.contextBudgetBySession[sessionId]);
  const breakdown = useChatStore((s) => s.contextBreakdownBySession[sessionId]);
  const totals = useChatStore((s) => s.sessionTotalsBySession[sessionId]);
  const model = useSessionModelStore(
    (s) => s.resolvedBySession[sessionId]?.model,
  );
  const { copied, copy } = useCopied();

  // Authoritative provider "used" wins over the chars/4 proxy; fall back to it,
  // then bail entirely when neither is known (before the first completed turn).
  const used = inputTokens ?? tokens;
  if (used == null) return null;

  // Ratio path (#598): only with a usable budget denominator. `Progress` clamps
  // 0–100, so an over-budget turn pegs at 100% rather than overflowing.
  const hasRatio = budget != null && budget > 0;
  const pct = hasRatio ? Math.round((used / budget) * 100) : null;

  const detail = hasRatio
    ? `Context usage: ${used.toLocaleString()} of ${budget.toLocaleString()} tokens (${pct}%)`
    : `Context usage: ${used.toLocaleString()} tokens`;

  // Segmented bar widths: the three components as shares of their own sum, so the
  // bar always fills even when Messages runs over the budget.
  const segSum = breakdown
    ? breakdown.systemTokens + breakdown.toolTokens + breakdown.messageTokens
    : 0;
  const segPct = (v: number) => (segSum > 0 ? (v / segSum) * 100 : 0);

  const copyJson = () => {
    const payload = {
      sessionId,
      model,
      used,
      budget,
      pctUsed: pct,
      breakdown,
      sessionTotals: totals,
    };
    void copy(JSON.stringify(payload, null, 2));
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          title={detail}
          aria-label={detail}
          className="flex shrink-0 items-center gap-1.5 rounded px-1 text-xs tabular-nums text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
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
          <span>{formatK(used)}</span>
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80">
        {/* Header: used / budget · %-before-compaction, with a copy-as-JSON button
            in the top-right corner. */}
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <div className="text-sm font-medium tabular-nums">
              {formatK(used)}
              {hasRatio ? (
                <span className="text-muted-foreground">
                  {" / "}
                  {formatK(budget)}
                </span>
              ) : null}
            </div>
            {hasRatio ? (
              <div className="text-xs text-muted-foreground">
                {pct}% before compaction
              </div>
            ) : null}
          </div>
          <button
            type="button"
            title={copied ? "Copied" : "Copy as JSON"}
            aria-label={copied ? "Copied" : "Copy as JSON"}
            onClick={copyJson}
            className="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
          >
            {copied ? (
              <Check className="size-3 text-emerald-500" />
            ) : (
              <Copy className="size-3" />
            )}
          </button>
        </div>

        {breakdown ? (
          <>
            {/* Segmented 3-tone bar: System (blue) / Tools (orange) / Messages
                (green). */}
            <div className="mt-3 flex h-1.5 overflow-hidden rounded-full bg-muted">
              <span
                className="bg-blue-500"
                style={{ width: `${segPct(breakdown.systemTokens)}%` }}
              />
              <span
                className="bg-orange-500"
                style={{ width: `${segPct(breakdown.toolTokens)}%` }}
              />
              <span
                className="bg-emerald-500"
                style={{ width: `${segPct(breakdown.messageTokens)}%` }}
              />
            </div>

            <div className="mt-3 space-y-1.5">
              <ComponentRow
                swatch="bg-blue-500"
                label="System prompt"
                tokens={breakdown.systemTokens}
                budget={budget}
              />
              <ComponentRow
                swatch="bg-orange-500"
                label="Tools"
                count={`${breakdown.toolSpecs} specs`}
                tokens={breakdown.toolTokens}
                budget={budget}
              />
              <ComponentRow
                swatch="bg-emerald-500"
                label="Messages"
                count={`${breakdown.messageCount} msgs`}
                tokens={breakdown.messageTokens}
                budget={budget}
              />
            </div>
          </>
        ) : null}

        {model ? (
          <div className="mt-3 flex items-center gap-2 border-t pt-2 text-xs">
            <span className="text-muted-foreground">Model</span>
            <span className="ml-auto min-w-0 truncate tabular-nums">
              {model}
            </span>
          </div>
        ) : null}

        {totals ? (
          <div className="mt-3 border-t pt-2">
            <div className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground/70">
              Session totals
            </div>
            <div className="mt-1 flex justify-between text-xs tabular-nums">
              <span className="text-muted-foreground">Token</span>
              <span>
                {formatK(totals.inputTokens)} in /{" "}
                {formatK(totals.outputTokens)} out
              </span>
            </div>
            <div className="flex justify-between text-xs tabular-nums">
              <span className="text-muted-foreground">Cache</span>
              <span>
                {formatK(totals.cacheReadTokens)} read /{" "}
                {formatK(totals.cacheWriteTokens)} written
              </span>
            </div>
          </div>
        ) : null}
      </PopoverContent>
    </Popover>
  );
}
