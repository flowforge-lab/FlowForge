// Schedule-builder ⇄ cron glue for the Scheduled new/edit form (#541, RFC 0017).
// The backend owns the human cadence *label* (via `preview_cadence`); this module
// only composes the 6-field cron expression the form sends to `create_scheduled_task`
// and parses one back so an existing task can seed the builder on edit.
//
// Cron is 6-field `sec min hour dom mon dow` — the form the `cron` crate parses and
// the shape `ScheduledTask.cron` already carries (e.g. the seeded `0 0 17 * * *`).
// The RFC §timezone table lists 5-field examples; we always pin the seconds field to
// `0`. Both functions are pure and total — they never throw.

/** The preset cadences offered by the segmented control. `custom` is a raw cron. */
export type Cadence = "hourly" | "daily" | "weekly" | "monthly" | "custom";

/** The "ON" choice for the Weekly preset, mapped to a cron day-of-week field. */
export type WeeklyOn =
  | "weekdays"
  | "weekends"
  | "sun"
  | "mon"
  | "tue"
  | "wed"
  | "thu"
  | "fri"
  | "sat";

/** Day-of-week cron field per Weekly "ON" choice (cron dow: 0/7 = Sunday). */
const WEEKLY_DOW: Record<WeeklyOn, string> = {
  weekdays: "1-5",
  weekends: "0,6",
  sun: "0",
  mon: "1",
  tue: "2",
  wed: "3",
  thu: "4",
  fri: "5",
  sat: "6",
};

/** Reverse of [`WEEKLY_DOW`] for parsing a cron dow field back to a Weekly "ON". */
const DOW_TO_WEEKLY: Record<string, WeeklyOn> = Object.fromEntries(
  Object.entries(WEEKLY_DOW).map(([on, dow]) => [dow, on as WeeklyOn]),
) as Record<string, WeeklyOn>;

/** The editable builder state behind the schedule section of the form. */
export interface ScheduleBuilderState {
  cadence: Cadence;
  /** "HH:MM" 24h — the TIME OF DAY for daily / weekly / monthly. */
  time: string;
  /** Weekly "ON" selection. */
  weeklyOn: WeeklyOn;
  /** Day of month (1–31) for the Monthly preset. */
  dayOfMonth: number;
  /** Raw 6-field expression for the Custom preset. */
  customCron: string;
}

/** A sensible starting point: daily at 09:00. */
export const DEFAULT_BUILDER_STATE: ScheduleBuilderState = {
  cadence: "daily",
  time: "09:00",
  weeklyOn: "weekdays",
  dayOfMonth: 1,
  customCron: "",
};

/** Parse "HH:MM" into `[hour, minute]`, clamping to valid ranges; falls back to
 *  09:00 on anything malformed so the composed cron is always well-formed. */
function parseTime(time: string): [number, number] {
  const m = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
  if (!m) return [9, 0];
  const hour = Math.min(23, Math.max(0, Number(m[1])));
  const minute = Math.min(59, Math.max(0, Number(m[2])));
  return [hour, minute];
}

/** Compose the 6-field cron expression for the current builder state. */
export function builderToCron(state: ScheduleBuilderState): string {
  if (state.cadence === "custom") return state.customCron.trim();
  const [hour, minute] = parseTime(state.time);
  switch (state.cadence) {
    case "hourly":
      // Top of every hour.
      return "0 0 * * * *";
    case "daily":
      return `0 ${minute} ${hour} * * *`;
    case "weekly":
      return `0 ${minute} ${hour} * * ${WEEKLY_DOW[state.weeklyOn]}`;
    case "monthly": {
      const dom = Math.min(31, Math.max(1, Math.round(state.dayOfMonth)));
      return `0 ${minute} ${hour} ${dom} * *`;
    }
  }
}

/** Format an `[hour, minute]` pair back into the "HH:MM" the time input expects. */
function formatTime(hour: number, minute: number): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(hour)}:${pad(minute)}`;
}

/** Best-effort parse of a stored cron back into builder state, for seeding the form
 *  on edit. Anything that doesn't match a known preset shape opens on Custom with the
 *  raw expression preserved, so no information is lost. Total — never throws. */
export function cronToBuilder(cron: string): ScheduleBuilderState {
  const asCustom = (): ScheduleBuilderState => ({
    ...DEFAULT_BUILDER_STATE,
    cadence: "custom",
    customCron: cron.trim(),
  });

  const fields = cron.trim().split(/\s+/);
  if (fields.length !== 6) return asCustom();
  const [sec, min, hour, dom, mon, dow] = fields;

  // Numeric minute/hour are required for every non-custom preset.
  const minNum = Number(min);
  const hourNum = Number(hour);
  const timeOk =
    /^\d+$/.test(min) && /^\d+$/.test(hour) && minNum <= 59 && hourNum <= 23;
  const time = timeOk
    ? formatTime(hourNum, minNum)
    : DEFAULT_BUILDER_STATE.time;

  // Hourly: top of every hour.
  if (
    sec === "0" &&
    min === "0" &&
    hour === "*" &&
    dom === "*" &&
    mon === "*" &&
    dow === "*"
  ) {
    return { ...DEFAULT_BUILDER_STATE, cadence: "hourly" };
  }
  if (!timeOk || mon !== "*") return asCustom();

  // Daily: every day at HH:MM.
  if (dom === "*" && dow === "*") {
    return { ...DEFAULT_BUILDER_STATE, cadence: "daily", time };
  }
  // Weekly: a recognized day-of-week set, every month.
  if (dom === "*" && dow !== "*") {
    const weeklyOn = DOW_TO_WEEKLY[dow];
    if (!weeklyOn) return asCustom();
    return { ...DEFAULT_BUILDER_STATE, cadence: "weekly", time, weeklyOn };
  }
  // Monthly: a numeric day-of-month, any weekday.
  if (/^\d+$/.test(dom) && dow === "*") {
    const dayOfMonth = Math.min(31, Math.max(1, Number(dom)));
    return { ...DEFAULT_BUILDER_STATE, cadence: "monthly", time, dayOfMonth };
  }
  return asCustom();
}
