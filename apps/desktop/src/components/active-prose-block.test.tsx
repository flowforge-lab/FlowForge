// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { userEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import {
  ACTIVE_PROSE_COLLAPSE_THRESHOLD,
  ActiveProseBlock,
} from "@/components/active-prose-block";

// Plain-text strings the Markdown renderer will pass through to the DOM
// unchanged, so DOM assertions can match them exactly.
const SHORT_TEXT = "Let me look that up.";
const LONG_TEXT = "x".repeat(ACTIVE_PROSE_COLLAPSE_THRESHOLD + 1);

// Long prose that simulates a multi-paragraph narration — the exact case the
// #864 review flagged with the 1000px `maxHeight` cap. We can't measure
// scrollHeight precisely in jsdom (it returns 0 for non-rendered content), but
// we can assert the *cap* is not a hard-coded constant: the inline `style`
// carries the measured height, and 0 is acceptable here (jsdom doesn't lay
// out); the real test is that the value is read from `naturalHeight` state,
// not 1000.
const VERY_LONG_TEXT = "y".repeat(2000);

// The chip is always in the DOM while the component is mounted — keeping it
// present enables the smooth collapse animation. Visible affordance is driven
// by `aria-expanded` + `aria-hidden` on the chip button; an `aria-hidden`
// button has no accessible name (the text inside is excluded from the a11y
// tree too), so the role-based query only finds it when it's visible. Use
// the `data-on-it` attribute selector as the stable hook for assertions that
// need to inspect the suppressed affordance.
function chipButton(): HTMLElement {
  return document.querySelector<HTMLElement>("button[data-on-it]")!;
}

afterEach(cleanup);

describe("ActiveProseBlock (#864)", () => {
  it("renders the full prose and keeps the chip hidden when the turn is settled", () => {
    render(<ActiveProseBlock text={LONG_TEXT} streaming={false} />);

    expect(screen.getByText(LONG_TEXT)).toBeTruthy();
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    // a11y: the chip is removed from the accessibility tree when its prose
    // is shown (#864 review #3).
    expect(chipButton().getAttribute("aria-hidden")).toBe("true");
    expect(chipButton().getAttribute("tabindex")).toBe("-1");
  });

  it("renders the full prose and keeps the chip hidden when streaming text is short", () => {
    render(<ActiveProseBlock text={SHORT_TEXT} streaming={true} />);

    expect(screen.getByText(SHORT_TEXT)).toBeTruthy();
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    expect(chipButton().getAttribute("aria-hidden")).toBe("true");
  });

  it("collapses long streaming prose to an 'On it' chip by default", () => {
    render(<ActiveProseBlock text={LONG_TEXT} streaming={true} />);

    expect(chipButton().getAttribute("aria-expanded")).toBe("false");
    // a11y: visible chip is in the accessibility tree, no tabindex penalty.
    expect(chipButton().getAttribute("aria-hidden")).toBe("false");
    expect(chipButton().getAttribute("tabindex")).toBeNull();
    // Prose content is still in the DOM (so the expand is instant) but hidden.
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeTruthy();
  });

  it("expands the live prose on click and keeps it expanded across token growth", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ActiveProseBlock text={LONG_TEXT} streaming={true} />,
    );

    await user.click(chipButton());

    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeNull();
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    // While streaming, the chip stays in the a11y tree and interactive so it
    // can be re-collapsed (#986) — it is only retired once the turn settles.
    expect(chipButton().getAttribute("aria-hidden")).toBe("false");
    expect(chipButton().getAttribute("tabindex")).toBeNull();

    // New tokens arrive — the user's choice must stick (#864 edge case).
    const GROWN = LONG_TEXT + " more text from the model";
    rerender(<ActiveProseBlock text={GROWN} streaming={true} />);
    expect(screen.getByText(GROWN).closest("[aria-hidden='true']")).toBeNull();
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
  });

  it("re-collapses a mid-stream expanded prose, repeatably (#986)", async () => {
    const user = userEvent.setup();
    render(<ActiveProseBlock text={LONG_TEXT} streaming={true} />);

    // Starts collapsed.
    expect(chipButton().getAttribute("aria-expanded")).toBe("false");
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeTruthy();

    // Expand.
    await user.click(chipButton());
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    // The collapse control must remain live while streaming.
    expect(chipButton().getAttribute("aria-hidden")).toBe("false");
    expect(chipButton().getAttribute("tabindex")).toBeNull();
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeNull();

    // Collapse back — the bug was that this was impossible.
    await user.click(chipButton());
    expect(chipButton().getAttribute("aria-expanded")).toBe("false");
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeTruthy();

    // And expand again — the toggle is repeatable.
    await user.click(chipButton());
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeNull();
  });

  it("dissolves the chip and shows the full prose when the turn settles", () => {
    const { rerender } = render(
      <ActiveProseBlock text={LONG_TEXT} streaming={true} />,
    );
    expect(chipButton().getAttribute("aria-expanded")).toBe("false");

    rerender(<ActiveProseBlock text={LONG_TEXT} streaming={false} />);
    expect(chipButton().getAttribute("aria-expanded")).toBe("true");
    expect(chipButton().getAttribute("aria-hidden")).toBe("true");
    expect(
      screen.getByText(LONG_TEXT).closest("[aria-hidden='true']"),
    ).toBeNull();
  });

  it("uses a measured max-height, not a hard 1000px cap, so long prose isn't clipped (#864 review #2)", () => {
    // The 1000px cap was the #864 review's #2 finding. The fix is to measure
    // `scrollHeight` of the content and use that. jsdom returns 0 for
    // scrollHeight on un-laid-out content, but the contract is that the
    // value comes from `naturalHeight` state, not the literal 1000 — the
    // inline style on the prose wrapper must not include the old magic
    // number.
    render(<ActiveProseBlock text={VERY_LONG_TEXT} streaming={true} />);

    // The chip is collapsed for very long streaming text, so the prose
    // wrapper is clipped. The inline style on the clipped wrapper drives
    // `maxHeight`. Assert the old magic number isn't there.
    const proseWrapper =
      screen.getByText(VERY_LONG_TEXT).parentElement?.parentElement;
    expect(proseWrapper).toBeTruthy();
    const styleAttr = proseWrapper?.getAttribute("style") ?? "";
    // No "1000" anywhere in the inline style — the cap must be data-driven.
    expect(styleAttr).not.toMatch(/1000/);
  });

  it("defaults to muted tone with no caret (the settled intermediate-prose call site)", () => {
    render(<ActiveProseBlock text={LONG_TEXT} streaming={false} />);

    const content = screen.getByText(LONG_TEXT).closest("[data-prose-content]");
    expect(content?.className).toMatch(/text-muted-foreground/);
    expect(content?.className).not.toMatch(/text-foreground\b/);
    expect(content?.className).not.toMatch(/ff-streaming-caret/);
  });

  it("switches to foreground tone with a caret when used for the live answer slot (#864)", () => {
    render(
      <ActiveProseBlock
        text={LONG_TEXT}
        streaming={true}
        tone="foreground"
        caret={true}
      />,
    );

    const content = screen.getByText(LONG_TEXT).closest("[data-prose-content]");
    expect(content?.className).toMatch(/text-foreground/);
    expect(content?.className).not.toMatch(/text-muted-foreground/);
    expect(content?.className).toMatch(/ff-streaming-caret/);
  });
});
