import { describe, expect, it } from "vitest";
import { formatMessageTime } from "./format-message-time";

describe("formatMessageTime", () => {
  it("formats a real epoch-ms timestamp as a short clock time", () => {
    // Assert shape (H:MM) rather than an exact string — the output is local-time
    // and locale-dependent, so an exact match would be TZ/CI-brittle.
    expect(formatMessageTime(Date.now())).toMatch(/\d{1,2}:\d{2}/);
  });

  it('returns "" for a missing/zero/sentinel or invalid timestamp', () => {
    expect(formatMessageTime(0)).toBe("");
    expect(formatMessageTime(-1)).toBe("");
    expect(formatMessageTime(Number.NaN)).toBe("");
    expect(formatMessageTime(undefined as unknown as number)).toBe("");
  });
});
