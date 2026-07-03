// @vitest-environment jsdom

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ContextGauge } from "@/components/context-gauge";
import { useChatStore } from "@/store/chat";

const SID = "s1";

// Seed only the two slices the gauge reads; everything else keeps the store's
// defaults. `tokens` of `null`/`undefined` models the no-estimate state; a `budget`
// of `null`/`undefined` models the count-only fallback (no ratio denominator yet).
function seed(tokens: number | null | undefined, budget?: number | null) {
  useChatStore.setState({
    contextTokensBySession: tokens == null ? {} : { [SID]: tokens },
    contextBudgetBySession: budget == null ? {} : { [SID]: budget },
  });
}

afterEach(() => {
  cleanup();
  seed(undefined);
});

describe("ContextGauge (#282)", () => {
  it("renders the formatted estimate once a turn has reported a count", () => {
    seed(12_345);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("≈12.3k")).not.toBeNull();
  });

  it("renders nothing when there is no estimate (undefined key)", () => {
    seed(undefined);
    const { container } = render(<ContextGauge sessionId={SID} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when the estimate is explicitly null", () => {
    // The store type holds `number`, but a Done without a count must read as
    // "no estimate" — guard the null case defensively all the same.
    useChatStore.setState({
      contextTokensBySession: { [SID]: null as unknown as number },
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
    expect(
      screen.getByTitle("Estimated context usage: 12,345 tokens"),
    ).not.toBeNull();
  });

  it("formats sub-1k counts exactly and ≥1M with an M suffix", () => {
    seed(840);
    const { rerender } = render(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("≈840")).not.toBeNull();

    useChatStore.setState({ contextTokensBySession: { [SID]: 1_500_000 } });
    rerender(<ContextGauge sessionId={SID} />);
    expect(screen.getByText("≈1.5M")).not.toBeNull();
  });
});

describe("ContextGauge — usage ratio (#598)", () => {
  it("renders a Progress bar at tokens/budget once a budget is reported", () => {
    seed(60_000, 120_000);
    render(<ContextGauge sessionId={SID} />);
    const bar = screen.getByRole("progressbar");
    // 60k of a 120k budget → 50%.
    expect(bar.getAttribute("aria-valuenow")).toBe("50");
    // The count text stays alongside the bar.
    expect(screen.getByText("≈60.0k")).not.toBeNull();
    expect(
      screen.getByTitle(
        "Estimated context usage: 60,000 of 120,000 tokens (50%)",
      ),
    ).not.toBeNull();
  });

  it("pegs the bar at 100% when usage exceeds the budget (title keeps the true %)", () => {
    seed(200_000, 120_000);
    render(<ContextGauge sessionId={SID} />);
    // The bar clamps to 100 even though the raw ratio is 167%.
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe(
      "100",
    );
    expect(
      screen.getByTitle(
        "Estimated context usage: 200,000 of 120,000 tokens (167%)",
      ),
    ).not.toBeNull();
  });

  it("falls back to count-only (no Progress bar) when the budget is absent", () => {
    seed(60_000);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("≈60.0k")).not.toBeNull();
  });

  it("falls back to count-only when the budget is zero (no divide-by-zero)", () => {
    seed(60_000, 0);
    render(<ContextGauge sessionId={SID} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("≈60.0k")).not.toBeNull();
  });
});
