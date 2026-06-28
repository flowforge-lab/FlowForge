import { describe, expect, it } from "vitest";

import {
  builderToCron,
  cronToBuilder,
  DEFAULT_BUILDER_STATE,
  type ScheduleBuilderState,
} from "@/lib/schedule-cron";

const state = (
  over: Partial<ScheduleBuilderState> = {},
): ScheduleBuilderState => ({
  ...DEFAULT_BUILDER_STATE,
  ...over,
});

describe("builderToCron", () => {
  it("hourly fires at the top of every hour (time ignored)", () => {
    expect(builderToCron(state({ cadence: "hourly", time: "13:45" }))).toBe(
      "0 0 * * * *",
    );
  });

  it("daily encodes the time of day", () => {
    expect(builderToCron(state({ cadence: "daily", time: "09:00" }))).toBe(
      "0 0 9 * * *",
    );
    expect(builderToCron(state({ cadence: "daily", time: "17:30" }))).toBe(
      "0 30 17 * * *",
    );
  });

  it("weekly encodes the day-of-week set", () => {
    expect(
      builderToCron(
        state({ cadence: "weekly", time: "09:00", weeklyOn: "weekdays" }),
      ),
    ).toBe("0 0 9 * * 1-5");
    expect(
      builderToCron(
        state({ cadence: "weekly", time: "09:00", weeklyOn: "weekends" }),
      ),
    ).toBe("0 0 9 * * 0,6");
    expect(
      builderToCron(
        state({ cadence: "weekly", time: "08:15", weeklyOn: "mon" }),
      ),
    ).toBe("0 15 8 * * 1");
  });

  it("monthly encodes the day of month, clamped to 1–31", () => {
    expect(
      builderToCron(
        state({ cadence: "monthly", time: "17:00", dayOfMonth: 17 }),
      ),
    ).toBe("0 0 17 17 * *");
    expect(
      builderToCron(
        state({ cadence: "monthly", time: "17:00", dayOfMonth: 99 }),
      ),
    ).toBe("0 0 17 31 * *");
  });

  it("custom passes the raw expression through, trimmed", () => {
    expect(
      builderToCron(
        state({ cadence: "custom", customCron: "  */5 * * * * *  " }),
      ),
    ).toBe("*/5 * * * * *");
  });

  it("falls back to 09:00 for a malformed time", () => {
    expect(builderToCron(state({ cadence: "daily", time: "nonsense" }))).toBe(
      "0 0 9 * * *",
    );
  });
});

describe("cronToBuilder", () => {
  it("round-trips every preset", () => {
    const cases: ScheduleBuilderState[] = [
      state({ cadence: "hourly" }),
      state({ cadence: "daily", time: "17:30" }),
      state({ cadence: "weekly", time: "09:00", weeklyOn: "weekdays" }),
      state({ cadence: "weekly", time: "08:15", weeklyOn: "sat" }),
      state({ cadence: "monthly", time: "17:00", dayOfMonth: 17 }),
    ];
    for (const s of cases) {
      expect(cronToBuilder(builderToCron(s))).toMatchObject({
        cadence: s.cadence,
      });
      // The composed cron the parsed state would re-emit must be stable.
      expect(builderToCron(cronToBuilder(builderToCron(s)))).toBe(
        builderToCron(s),
      );
    }
  });

  it("maps the seeded weekly-digest cron to a weekly Monday", () => {
    const b = cronToBuilder("0 0 9 * * 1");
    expect(b.cadence).toBe("weekly");
    expect(b.weeklyOn).toBe("mon");
    expect(b.time).toBe("09:00");
  });

  it("opens on Custom for a non-preset expression", () => {
    const b = cronToBuilder("0 0 9 * * 1,3,5");
    expect(b.cadence).toBe("custom");
    expect(b.customCron).toBe("0 0 9 * * 1,3,5");
  });

  it("opens on Custom for a non-6-field expression", () => {
    expect(cronToBuilder("0 9 * * *").cadence).toBe("custom");
    expect(cronToBuilder("").cadence).toBe("custom");
  });
});
