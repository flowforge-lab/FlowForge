// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";

// Substantive intermediate prose (#687): long (≥LONG), unformatted single paragraphs
// with no operational prefix, so `segmentTurn` hoists each chunk to a top-level block
// rather than folding it into the step group as a thought. Kept unformatted (no inline
// code / bold / breaks) so the Markdown renderer emits one plain text node the DOM
// assertions below can match exactly.
const PROSE_1 =
  "Scanned the workspace layout and confirmed that the composer component and its supporting store are the only places the intermediate rendering is assembled, which means the follow-up search can be narrowed to those two files and their tests without widening the change surface into the shared contract or the generated bindings the two engineers agreed to keep stable.";
const PROSE_2 =
  "The grep sweep across the source tree returned a single call site inside the step group, so the fix stays local to that one component and the unit tests around it, and there is no need to introduce a parallel helper or a new abstraction when the existing windowing utility can be generalized to operate over the interleaved items instead of only the tool steps it counts today.";

function msg(
  partial: Partial<Message> & Pick<Message, "id" | "role">,
): Message {
  return { sessionId: SID, content: "", createdAt: 1, ...partial };
}

// A settled, reloaded two-iteration turn (no live steps): each assistant message
// carries its intermediate prose in `content` + one tool call, and the final
// assistant message carries the answer. Mirrors the persisted transcript shape.
function seedMultiIterationTurn() {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {
      [SID]: [
        msg({ id: "u1", role: "user", content: "go" }),
        msg({
          id: "a1",
          role: "assistant",
          content: PROSE_1,
          toolCalls: [{ id: "c1", name: "view", arguments: "{}" }],
        }),
        msg({ id: "t1", role: "tool", toolCallId: "c1", content: "result 1" }),
        msg({
          id: "a2",
          role: "assistant",
          content: PROSE_2,
          toolCalls: [{ id: "c2", name: "grep", arguments: "{}" }],
        }),
        msg({ id: "t2", role: "tool", toolCallId: "c2", content: "result 2" }),
        msg({ id: "a3", role: "assistant", content: "Here is the answer." }),
      ],
    },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

// A streaming turn whose final live activity is a tool step (not prose) — the
// `prose → steps` order. The pre-fix code marked the last prose as `streaming`,
// which collapsed a settled prose to "On it" while the steps below were
// actually live. The fix gates the streaming flag on prose being the last
// segment overall (#864 review #1).
function seedProseFollowedByLiveSteps() {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {
      [SID]: [
        msg({ id: "u1", role: "user", content: "go" }),
        msg({
          id: "a1",
          role: "assistant",
          content: PROSE_1,
          toolCalls: [{ id: "c1", name: "view", arguments: "{}" }],
        }),
        msg({ id: "t1", role: "tool", toolCallId: "c1", content: "result 1" }),
        msg({
          id: "a2",
          role: "assistant",
          content: PROSE_2,
        }),
      ],
    },
    // a2 is in flight, with a live step pending. a2 has no prose item
    // (finalAssistant), so the only prose is PROSE_1 (already settled).
    streamingBySession: { [SID]: "a2" },
    turnStartBySession: { [SID]: 1000 },
    turnStartByMessage: { a2: 1000 },
    toolStepsByMessage: {
      a2: [
        {
          callId: "c2",
          tool: "grep",
          args: {},
          status: "running",
          startedAt: 1000,
        },
      ],
    },
    reasoningByMessage: {},
  });
}

// Collapsed "N steps" group headers (StepGroup's toggle button).
function stepHeaders(): HTMLElement[] {
  return screen
    .getAllByRole("button", { expanded: false })
    .filter((b) => /\d+\s+steps?/.test(b.textContent ?? ""));
}

describe("ChatView intermediate prose (#619)", () => {
  beforeEach(() => seedMultiIterationTurn());
  afterEach(() => {
    cleanup();
    useChatStore.setState({ messagesBySession: {} });
  });

  it("shows intermediate prose at top level without expanding any group", () => {
    render(<ChatView />);

    // Both intermediate prose blocks are visible even though the turn is settled
    // and every step group is collapsed.
    expect(screen.getByText(PROSE_1)).toBeTruthy();
    expect(screen.getByText(PROSE_2)).toBeTruthy();

    const headers = stepHeaders();
    expect(headers.length).toBeGreaterThanOrEqual(2);
    for (const h of headers) {
      expect(h.getAttribute("aria-expanded")).toBe("false");
    }
  });

  it("renders one collapsed '1 step' group per iteration, in chronological order", () => {
    const { container } = render(<ChatView />);

    const headers = stepHeaders();
    expect(headers.length).toBe(2);
    for (const h of headers) {
      expect(h.textContent).toMatch(/1 step/);
    }

    // Chronological order: prose → group → prose → group, top to bottom.
    // Read the flattened text (DOM order) so the assertion doesn't couple to the
    // element the Markdown renderer wraps prose in (#629).
    const body = container.textContent ?? "";
    const sequence = [
      PROSE_1, // intermediate prose 1
      "1 step", // first group header
      PROSE_2, // intermediate prose 2
      "1 step", // second group header
    ];
    let cursor = -1;
    for (const anchor of sequence) {
      const idx = body.indexOf(anchor, cursor + 1);
      expect(idx).toBeGreaterThan(cursor);
      cursor = idx;
    }
  });

  it("renders settled intermediate prose as full markdown with hidden chips (#864)", () => {
    // The turn isn't streaming, so every ActiveProseBlock chip must be in the
    // "expanded" (hidden) state and the full prose must remain visible.
    // Regression guard for the #864 streaming-collapse work — settled turns
    // must be unchanged.
    render(<ChatView />);

    expect(screen.getByText(PROSE_1)).toBeTruthy();
    expect(screen.getByText(PROSE_2)).toBeTruthy();
    // Two prose segments → two chip buttons, each in the hidden-expanded
    // state. The chip is suppressed from the a11y tree in that state, so we
    // look it up via the `data-on-it` attribute instead of the (now empty)
    // accessible name.
    const chips = Array.from(
      document.querySelectorAll<HTMLElement>("button[data-on-it]"),
    );
    expect(chips.length).toBe(2);
    for (const chip of chips) {
      expect(chip.getAttribute("aria-expanded")).toBe("true");
    }
  });
});

describe("ChatView active prose (#864 streaming edge case)", () => {
  afterEach(() => {
    cleanup();
    useChatStore.setState({ messagesBySession: {} });
  });

  it("does not collapse a settled prose to 'On it' when the final live activity is a step", () => {
    // Turn shape: prose → steps → (streaming) prose? No — in this seed, a2 is
    // the streaming message with a live step but no content. The only prose
    // segment is PROSE_1, which is already settled (the model moved on to
    // running a tool). The fix must NOT mark PROSE_1 as streaming, even
    // though it's the last prose segment.
    seedProseFollowedByLiveSteps();
    render(<ChatView />);

    // PROSE_1 should be fully visible (chip hidden), not collapsed to "On it".
    const prose = screen.getByText(PROSE_1);
    expect(prose.closest("[aria-hidden='true']")).toBeNull();
    // Chip button is present in the DOM (so the layout doesn't reflow) but
    // its a11y is fully suppressed: aria-expanded=true (prose shown) and
    // aria-hidden=true (removed from the a11y tree, per #864 review #3).
    // The chip is excluded from the a11y tree in that state, so its
    // accessible name is empty — look it up via the `data-on-it` attribute.
    const chip = document.querySelector<HTMLElement>("button[data-on-it]")!;
    expect(chip.getAttribute("aria-expanded")).toBe("true");
    expect(chip.getAttribute("aria-hidden")).toBe("true");
  });

  it("collapses the live answer slot to 'On it' once it has its own recorded tool call", () => {
    // a2 is this same seed's *streaming* message: its content (PROSE_2) is
    // still growing in the answer slot, and it already has its own live step
    // (the `grep` call) recorded — so it's guaranteed not the final answer,
    // even though nothing has superseded it yet. This is the actual live,
    // in-flight case the issue describes; unlike a promoted `prose` segment
    // (always already-settled by the time it exists, see turn-groups.ts),
    // this is the one place `streaming` can genuinely be true.
    seedProseFollowedByLiveSteps();
    render(<ChatView />);

    const chips = Array.from(
      document.querySelectorAll<HTMLElement>("button[data-on-it]"),
    );
    // PROSE_1's (settled, hidden) chip plus PROSE_2's (live, collapsed) chip.
    expect(chips.length).toBe(2);
    const liveChip = chips[1];
    expect(liveChip.getAttribute("aria-expanded")).toBe("false");
    expect(liveChip.getAttribute("aria-hidden")).toBe("false");
    // PROSE_2's text is in the DOM (for the smooth expand) but clipped.
    expect(
      screen.getByText(PROSE_2).closest("[aria-hidden='true']"),
    ).toBeTruthy();
  });
});
