import { describe, expect, it } from "vitest";

import { isCappedNotice } from "@/store/capped-turn";

describe("isCappedNotice (#513)", () => {
  it("matches the reason-bearing stop notices the agent loop writes", () => {
    expect(isCappedNotice("[stopped: reached tool-call limit]")).toBe(true);
    expect(
      isCappedNotice(
        "[stopped: repeated the identical `read` tool call 3 times without making progress]",
      ),
    ).toBe(true);
    expect(
      isCappedNotice("[stopped: the model returned an empty response]"),
    ).toBe(true);
  });

  it("tolerates surrounding whitespace", () => {
    expect(isCappedNotice("  [stopped: reached tool-call limit]\n")).toBe(true);
  });

  it("excludes a bare [stopped] (deliberate user cancel)", () => {
    expect(isCappedNotice("[stopped]")).toBe(false);
  });

  it("does not match a normal assistant answer", () => {
    expect(isCappedNotice("Here's the summary of what I did.")).toBe(false);
    expect(isCappedNotice("")).toBe(false);
    // A notice mentioned mid-sentence is not a stop marker.
    expect(
      isCappedNotice("The build [stopped: ...] message means the cap was hit."),
    ).toBe(false);
  });
});
