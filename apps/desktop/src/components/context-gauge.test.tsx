// @vitest-environment jsdom

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ContextGauge } from "@/components/context-gauge";
import { useChatStore } from "@/store/chat";

const SID = "s1";

// Seed only the context-tokens slice the gauge reads; everything else keeps the
// store's defaults. `tokens` of `null`/`undefined` models the no-estimate state.
function seed(tokens: number | null | undefined) {
  useChatStore.setState({
    contextTokensBySession: tokens == null ? {} : { [SID]: tokens },
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
