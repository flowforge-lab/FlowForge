// @vitest-environment jsdom

import {
  render,
  screen,
  cleanup,
  fireEvent,
  act,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ContextGauge } from "@/components/context-gauge";
import { useChatStore } from "@/store/chat";
import { useSessionModelStore } from "@/store/session-model";
import type { ContextBreakdown, ResolvedModel, TurnUsage } from "@/bindings";

const SID = "s1";

// Reset every slice the gauge reads back to empty. `tokens`/`budget` cover the
// legacy count + ratio paths; the popover slices default to empty unless a test
// seeds them via `seedPopover`.
function seed(tokens: number | null | undefined, budget?: number | null) {
  useChatStore.setState({
    contextTokensBySession: tokens == null ? {} : { [SID]: tokens },
    contextBudgetBySession: budget == null ? {} : { [SID]: budget },
    contextInputTokensBySession: {},
    contextUsageBySession: {},
    contextBreakdownBySession: {},
    sessionTotalsBySession: {},
    ttftBySession: {},
    promptLatencyBySession: {},
    tier2MsBySession: {},
  });
  useSessionModelStore.setState({ resolvedBySession: {} });
}

function seedPopover(opts: {
  inputTokens?: number;
  breakdown?: ContextBreakdown;
  totals?: TurnUsage;
  usage?: TurnUsage;
  contextWindow?: number;
  model?: string;
  ttft?: number;
  promptLatencyMs?: number;
  tier2Ms?: number;
}) {
  const inputs: Record<string, number> =
    opts.inputTokens == null ? {} : { [SID]: opts.inputTokens };
  const breakdowns: Record<string, ContextBreakdown> = opts.breakdown
    ? { [SID]: opts.breakdown }
    : {};
  const totals: Record<string, TurnUsage> = opts.totals
    ? { [SID]: opts.totals }
    : {};
  useChatStore.setState({
    contextInputTokensBySession: inputs,
    contextUsageBySession: opts.usage ? { [SID]: opts.usage } : {},
    contextBreakdownBySession: breakdowns,
    sessionTotalsBySession: totals,
    ttftBySession: opts.ttft == null ? {} : { [SID]: opts.ttft },
    promptLatencyBySession:
      opts.promptLatencyMs == null ? {} : { [SID]: opts.promptLatencyMs },
    tier2MsBySession: opts.tier2Ms == null ? {} : { [SID]: opts.tier2Ms },
  });
  if (opts.model) {
    const resolved: ResolvedModel = {
      connection: "c1",
      model: opts.model,
      supportsVision: false,
      supportsDocuments: false,
      contextWindow: opts.contextWindow ?? null,
      trainedContextWindow: null,
      contextWindowSource: null,
    };
    useSessionModelStore.setState({ resolvedBySession: { [SID]: resolved } });
  }
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  seed(undefined);
});

describe("ContextGauge (#282)", () => {
  it("renders the formatted estimate once a turn has reported a count", () => {
    seed(12_345);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("12.3K")).not.toBeNull();
  });

  it("renders nothing when there is no estimate (undefined key)", () => {
    seed(undefined);
    const { container } = render(<ContextGauge sessionId={SID} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when the estimate is explicitly null", () => {
    useChatStore.setState({
      contextTokensBySession: { [SID]: null as unknown as number },
      contextInputTokensBySession: {},
    });
    const { container } = render(<ContextGauge sessionId={SID} />);
    expect(container.firstChild).toBeNull();
  });

  it("is scoped per session — an estimate for another session does not show", () => {
    seed(9_000);
    const { container } = render(<ContextGauge sessionId="other" />);
    expect(container.firstChild).toBeNull();
  });

  it("exposes the exact count via the title/aria-label", () => {
    seed(12_345);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.getByTitle("Context usage: 12,345 tokens")).not.toBeNull();
  });

  it("formats sub-1k counts exactly and stays in K past a million", () => {
    seed(840);
    const { rerender } = render(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("840")).not.toBeNull();

    useChatStore.setState({ contextTokensBySession: { [SID]: 1_500_000 } });
    rerender(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("1500K")).not.toBeNull();
  });
});

describe("ContextGauge — usage ratio (#598)", () => {
  it("renders a Progress bar at used/budget once a budget is reported", () => {
    seed(60_000, 120_000);
    render(<ContextGauge sessionId={SID} />);
    const bar = screen.getByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("50");
    expect(screen.getByText("60K")).not.toBeNull();
    expect(
      screen.getByTitle("Context usage: 60,000 of 120,000 tokens (50%)"),
    ).not.toBeNull();
  });

  it("pegs the bar at 100% when usage exceeds the budget (title keeps the true %)", () => {
    seed(200_000, 120_000);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe(
      "100",
    );
    expect(
      screen.getByTitle("Context usage: 200,000 of 120,000 tokens (167%)"),
    ).not.toBeNull();
  });

  it("falls back to count-only (no Progress bar) when the budget is absent", () => {
    seed(60_000);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("60K")).not.toBeNull();
  });

  it("falls back to count-only when the budget is zero (no divide-by-zero)", () => {
    seed(60_000, 0);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("60K")).not.toBeNull();
    expect(screen.getByTitle("Context usage: 60,000 tokens")).not.toBeNull();
  });
});

describe("ContextGauge — popover (#931)", () => {
  const BREAKDOWN: ContextBreakdown = {
    systemTokens: 12_000,
    toolTokens: 2_900,
    toolSpecs: 1,
    verbatimTokens: 200_000,
    wireTokens: 158_000,
    messageCount: 122,
  };
  const TOTALS: TurnUsage = {
    inputTokens: 5_760_000,
    outputTokens: 45_000,
    cacheReadTokens: 5_506_000,
    cacheWriteTokens: 254_000,
  };

  it("prefers breakdown sum over the chars/4 proxy for the pill (#945)", () => {
    seed(999_000, 150_000);
    seedPopover({ inputTokens: 173_000, breakdown: BREAKDOWN });
    render(<ContextGauge sessionId={SID} />);
    // breakdown sum = 12000+2900+158000 = 172900, displayed as 172.9K
    expect(screen.getByText("172.9K")).not.toBeNull();
    expect(screen.queryByText("999K")).toBeNull();
  });

  it("opens on click and renders the breakdown rows, model, and session totals", () => {
    seed(173_000, 150_000);
    seedPopover({
      inputTokens: 173_000,
      breakdown: BREAKDOWN,
      totals: TOTALS,
      model: "claude-opus-4-8",
    });
    render(<ContextGauge sessionId={SID} />);

    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));

    const panel = document.querySelector('[data-slot="popover-content"]');
    expect(panel).not.toBeNull();
    const text = panel?.textContent ?? "";
    expect(text).toContain("estimate");
    expect(text).toContain("System prompt");
    expect(text).toContain("1 spec");
    expect(text).not.toContain("1 specs");
    expect(text).toContain("122 msgs");
    expect(text).toContain("claude-opus-4-8");
    // SESSION TOTALS block, formatted in K.
    expect(text).toContain("5760K in / 45K out");
    expect(text).toContain("5506K read / 254K written");
  });

  it("copies the full context-usage state as pretty JSON and flashes Copied", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    seed(173_000, 150_000);
    seedPopover({
      inputTokens: 173_000,
      breakdown: BREAKDOWN,
      totals: TOTALS,
      model: "claude-opus-4-8",
      ttft: 8_200,
      promptLatencyMs: 6_000,
      tier2Ms: 800,
    });
    render(<ContextGauge sessionId={SID} />);

    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const copyBtn = screen.getByRole("button", { name: "Copy as JSON" });

    await act(async () => {
      fireEvent.click(copyBtn);
    });

    expect(writeText).toHaveBeenCalledTimes(1);
    const payload = JSON.parse(writeText.mock.calls[0][0] as string);
    // used = breakdown sum (12000+2900+158000 = 172900), pct = round(172900/150000*100) = 115
    expect(payload).toMatchObject({
      sessionId: SID,
      model: "claude-opus-4-8",
      used: 172_900,
      denom: 150_000,
      pctUsed: 115,
      mode: "estimate",
      ttft: 8_200,
      promptLatencyMs: 6_000,
      tier2Ms: 800,
      breakdown: BREAKDOWN,
      sessionTotals: TOTALS,
    });
    // Pretty-printed (2-space indent).
    expect(writeText.mock.calls[0][0]).toContain('\n  "sessionId"');

    // Transient "Copied" state, reverting after 1500ms.
    expect(screen.getByRole("button", { name: "Copied" })).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    expect(screen.getByRole("button", { name: "Copy as JSON" })).not.toBeNull();
  });

  it("pluralizes the tool specs count (singular vs plural)", () => {
    // Radix's Popover keeps its own open/closed state across a `rerender`, so a
    // second click after new data would toggle it closed rather than open — mount
    // fresh per case instead.
    seed(173_000, 150_000);
    seedPopover({ breakdown: { ...BREAKDOWN, toolSpecs: 1 } });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    let text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("1 spec");
    expect(text).not.toContain("1 specs");
    cleanup();

    seed(173_000, 150_000);
    seedPopover({ breakdown: { ...BREAKDOWN, toolSpecs: 3 } });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("3 specs");
  });

  it("renders the header/pill but omits breakdown + totals when only a proxy count exists", () => {
    seed(60_000, 120_000);
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("of budget (estimate)");
    expect(text).not.toContain("System prompt");
    expect(text).not.toContain("Session totals");
  });

  it("shows the prefill-share breakdown under TTFT when both latencies are present (#960)", () => {
    seed(173_000, 150_000);
    seedPopover({
      breakdown: BREAKDOWN,
      ttft: 8_200,
      promptLatencyMs: 6_000,
    });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("TTFT");
    expect(text).toContain("8.2s");
    expect(text).toContain("prompt 6.0s (73%) · other 2.2s");
  });

  it("shows plain TTFT without a prefill-share line when promptLatencyMs is absent (#960)", () => {
    seed(173_000, 150_000);
    seedPopover({ breakdown: BREAKDOWN, ttft: 6_006 });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("TTFT");
    expect(text).toContain("6.0s");
    expect(text).not.toContain("prompt ");
    expect(text).not.toContain("other ");
  });

  it("shows the per-phase attribution row when a compaction phase fired (#971)", () => {
    seed(173_000, 150_000);
    seedPopover({
      breakdown: BREAKDOWN,
      ttft: 8_200,
      promptLatencyMs: 6_000,
      tier2Ms: 800,
    });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("main 6.0s · summarize 800ms");
  });

  it("omits phases the turn did not run (#971)", () => {
    seed(173_000, 150_000);
    seedPopover({
      breakdown: BREAKDOWN,
      ttft: 12_000,
      promptLatencyMs: 3,
      tier2Ms: 9_000,
    });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("main 3ms · summarize 9.0s");
    expect(text).not.toContain("flush ");
  });

  it("shows no per-phase row when neither compaction phase fired (#971)", () => {
    seed(173_000, 150_000);
    seedPopover({ breakdown: BREAKDOWN, ttft: 8_200, promptLatencyMs: 6_000 });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context usage/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).not.toContain("flush ");
    expect(text).not.toContain("summarize ");
    // The prefill-share line's "main" prefix must not leak in without a phase.
    expect(text).not.toContain("main ");
  });

  it("context fill = wire vs real window (not cumulative usage), ≤100% (#1023)", () => {
    // Fill numerator is the breakdown's last-request wire (system+tools+wire =
    // 12k+2.9k+158k = 172.9k) against the real window (200k) = 86%. Provider usage is
    // cumulative cost throughput (cacheRead 150k can exceed a single request) and must
    // NOT be the fill numerator — it only drives the separate cache-hit line.
    seedPopover({
      breakdown: BREAKDOWN,
      usage: {
        inputTokens: 20_000,
        outputTokens: 1_000,
        cacheReadTokens: 150_000,
        cacheWriteTokens: 10_000,
      },
      contextWindow: 200_000,
      model: "claude",
    });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("86%");
    expect(text).toContain("of context window");
    expect(text).not.toContain("estimate");
    // cacheRead / (input+cacheRead+cacheWrite) = 150k/180k = 83%.
    expect(text).toContain("83% served from cache");
  });

  it("falls back to the soft budget (labeled estimate) when no real window (#1023)", () => {
    // No contextWindow → denominator falls back to the soft compaction budget,
    // labeled estimate. No usage → no cache-hit line.
    seed(null, 200_000);
    seedPopover({ breakdown: BREAKDOWN });
    render(<ContextGauge sessionId={SID} />);
    fireEvent.click(screen.getByRole("button", { name: /context/i }));
    const text =
      document.querySelector('[data-slot="popover-content"]')?.textContent ??
      "";
    expect(text).toContain("estimate");
    expect(text).not.toContain("served from cache");
  });
});
