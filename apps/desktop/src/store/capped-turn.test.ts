import { describe, expect, it } from "vitest";

import { isResumableStopNotice } from "@/store/capped-turn";

describe("isResumableStopNotice (#513/#636)", () => {
  it("matches the reason-bearing stop notices the agent loop writes", () => {
    expect(isResumableStopNotice("[stopped: reached tool-call limit]")).toBe(
      true,
    );
    expect(
      isResumableStopNotice(
        "[stopped: repeated the identical `read` tool call 3 times without making progress]",
      ),
    ).toBe(true);
    expect(
      isResumableStopNotice("[stopped: the model returned an empty response]"),
    ).toBe(true);
  });

  it("tolerates surrounding whitespace", () => {
    expect(
      isResumableStopNotice("  [stopped: reached tool-call limit]\n"),
    ).toBe(true);
  });

  it("matches a bare [stopped] (user cancel via Stop) — now resumable (#636)", () => {
    expect(isResumableStopNotice("[stopped]")).toBe(true);
    expect(isResumableStopNotice("  [stopped]\n")).toBe(true);
  });

  it("does not match a normal assistant answer", () => {
    expect(isResumableStopNotice("Here's the summary of what I did.")).toBe(
      false,
    );
    expect(isResumableStopNotice("")).toBe(false);
    // A notice mentioned mid-sentence is not a stop marker.
    expect(
      isResumableStopNotice(
        "The build [stopped: ...] message means the cap was hit.",
      ),
    ).toBe(false);
  });
});
