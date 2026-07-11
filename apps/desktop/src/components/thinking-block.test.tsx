// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  ThinkingBlock,
  resolveThinkingOpen,
} from "@/components/thinking-block";

afterEach(() => {
  cleanup();
});

describe("resolveThinkingOpen", () => {
  it("defaults collapsed when untouched, even mid-stream (#205)", () => {
    expect(resolveThinkingOpen(null)).toBe(false);
  });

  it("respects an explicit user toggle open", () => {
    expect(resolveThinkingOpen(true)).toBe(true);
  });

  it("respects an explicit user toggle closed", () => {
    expect(resolveThinkingOpen(false)).toBe(false);
  });
});

// Review follow-up on #875 (PR #901): the collapsed 120-char preview is a
// *sibling* of the expanded body, not a descendant, so it needs its own
// `data-skip-find` marker. Without it, a query matching only the preview's
// truncated text was paintable (a visible highlight) even though the data
// model excludes all reasoning from the `n of m` count — the exact
// painted-but-not-counted divergence #875 set out to eliminate.
describe("ThinkingBlock — data-skip-find coverage (#875 review)", () => {
  const reasoning =
    "The user wants me to run three git commands: git status, git diff, git log.";

  it("marks the collapsed preview text as skip-find", () => {
    render(<ThinkingBlock reasoning={reasoning} streaming={false} hasAnswer />);
    const preview = screen.getByText(/git status/);
    expect(preview.closest("[data-skip-find]")).not.toBeNull();
  });

  it("marks the expanded body as skip-find once opened, with no un-marked duplicate", async () => {
    const user = userEvent.setup();
    render(<ThinkingBlock reasoning={reasoning} streaming={false} hasAnswer />);
    await user.click(screen.getByRole("button", { name: /thinking/i }));

    // The preview is gone and only the expanded body remains — exactly one
    // occurrence of the text, and it's skip-find-marked.
    const matches = screen.getAllByText(/git status/);
    expect(matches).toHaveLength(1);
    expect(matches[0].closest("[data-skip-find]")).not.toBeNull();
  });
});
