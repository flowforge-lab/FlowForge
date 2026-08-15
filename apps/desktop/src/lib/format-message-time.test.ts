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

describe("formatMessageTime — date prefix (#1259)", () => {
  // Fri Jul 10 2026, 15:00 local (same anchor as sessions.test.ts).
  const now = new Date(2026, 6, 10, 15, 0, 0).getTime();

  it("keeps today's messages time-only, with no date prefix", () => {
    const earlyThisMorning = new Date(2026, 6, 10, 8, 5, 0).getTime();
    const out = formatMessageTime(earlyThisMorning, now);
    expect(out).toMatch(/^\d{1,2}:\d{2}/);
    // No day bucket leaked in — neither a date nor the word "Today".
    expect(out).not.toContain("Today");
    expect(out).not.toContain("Jul");
  });

  it('prefixes the previous calendar day with "Yesterday"', () => {
    const yesterdayAfternoon = new Date(2026, 6, 9, 14, 0, 0).getTime();
    expect(formatMessageTime(yesterdayAfternoon, now)).toMatch(/^Yesterday, /);
  });

  it('still reads "Yesterday" close to midnight (local day, not a rolling 24h)', () => {
    const lateLastNight = new Date(2026, 6, 9, 23, 55, 0).getTime();
    expect(formatMessageTime(lateLastNight, now)).toMatch(/^Yesterday, /);
  });

  it("uses a short month/day for earlier this year, with no year", () => {
    const earlierThisYear = new Date(2026, 2, 3, 12, 0, 0).getTime();
    const out = formatMessageTime(earlierThisYear, now);
    expect(out).toContain("Mar 3");
    expect(out).not.toMatch(/\d{4}/);
  });

  it("includes the year when it differs from the current year", () => {
    const lastYear = new Date(2024, 6, 3, 12, 0, 0).getTime();
    const out = formatMessageTime(lastYear, now);
    expect(out).toContain("Jul 3");
    expect(out).toContain("2024");
  });

  it("keeps the empty-string guard regardless of `now`", () => {
    expect(formatMessageTime(0, now)).toBe("");
    expect(formatMessageTime(Number.NaN, now)).toBe("");
  });
});
