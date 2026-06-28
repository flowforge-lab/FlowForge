import { Loader2 } from "@/components/ui/icon";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { cn } from "@/lib/utils";
import type {
  Cadence,
  ScheduleBuilderState,
  WeeklyOn,
} from "@/lib/schedule-cron";

/** Live cadence preview state, owned by the form and rendered here. */
export type CadencePreview =
  | { state: "loading" }
  | { state: "ok"; label: string }
  | { state: "error"; message: string };

const CADENCE_OPTIONS: ReadonlyArray<{ value: Cadence; label: string }> = [
  { value: "hourly", label: "Hourly" },
  { value: "daily", label: "Daily" },
  { value: "weekly", label: "Weekly" },
  { value: "monthly", label: "Monthly" },
  { value: "custom", label: "Custom" },
];

const WEEKLY_ON_OPTIONS: ReadonlyArray<{ value: WeeklyOn; label: string }> = [
  { value: "weekdays", label: "Weekdays (Mon–Fri)" },
  { value: "weekends", label: "Weekends (Sat–Sun)" },
  { value: "mon", label: "Mondays" },
  { value: "tue", label: "Tuesdays" },
  { value: "wed", label: "Wednesdays" },
  { value: "thu", label: "Thursdays" },
  { value: "fri", label: "Fridays" },
  { value: "sat", label: "Saturdays" },
  { value: "sun", label: "Sundays" },
];

function fieldLabelClass() {
  return "text-[11px] font-semibold tracking-wide text-muted-foreground uppercase";
}

/**
 * Schedule builder (#541, RFC 0017): a cadence segmented control over a TIME OF DAY
 * + ON selector (or a raw cron field on Custom), with a live cadence preview the
 * form computes server-side via `preview_cadence`. Presentational — `value` in,
 * `onChange` out; the parent owns the cron composition and the preview fetch.
 */
export function ScheduleBuilder({
  value,
  onChange,
  preview,
}: {
  value: ScheduleBuilderState;
  onChange: (next: ScheduleBuilderState) => void;
  preview: CadencePreview | null;
}) {
  const set = (patch: Partial<ScheduleBuilderState>) =>
    onChange({ ...value, ...patch });
  const showTime = value.cadence !== "hourly" && value.cadence !== "custom";

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <span className={fieldLabelClass()}>Schedule</span>
        <SegmentedControl
          label="Cadence"
          options={CADENCE_OPTIONS}
          value={value.cadence}
          onValueChange={(cadence) => set({ cadence })}
        />
      </div>

      {value.cadence === "custom" ? (
        <div className="space-y-1.5">
          <label htmlFor="cron" className={fieldLabelClass()}>
            Cron expression
          </label>
          <Input
            id="cron"
            value={value.customCron}
            placeholder="0 0 9 * * 1-5"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            onChange={(e) => set({ customCron: e.target.value })}
          />
          <p className="text-[11px] text-muted-foreground">
            6-field <span className="font-mono">sec min hour dom mon dow</span>,
            evaluated in your local time.
          </p>
        </div>
      ) : (
        <div className="flex flex-wrap items-end gap-3">
          {showTime ? (
            <div className="space-y-1.5">
              <label htmlFor="time" className={fieldLabelClass()}>
                Time of day
              </label>
              <Input
                id="time"
                type="time"
                value={value.time}
                className="w-32"
                onChange={(e) => set({ time: e.target.value })}
              />
            </div>
          ) : null}

          {value.cadence === "weekly" ? (
            <div className="min-w-44 flex-1 space-y-1.5">
              <span className={fieldLabelClass()}>On</span>
              <Select
                value={value.weeklyOn}
                onValueChange={(weeklyOn) =>
                  set({ weeklyOn: weeklyOn as WeeklyOn })
                }
              >
                <SelectTrigger aria-label="On">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {WEEKLY_ON_OPTIONS.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          ) : null}

          {value.cadence === "monthly" ? (
            <div className="space-y-1.5">
              <label htmlFor="dom" className={fieldLabelClass()}>
                Day of month
              </label>
              <Input
                id="dom"
                type="number"
                min={1}
                max={31}
                value={value.dayOfMonth}
                className="w-24"
                onChange={(e) =>
                  set({ dayOfMonth: Number(e.target.value) || 1 })
                }
              />
            </div>
          ) : null}
        </div>
      )}

      <CadencePreviewLine preview={preview} />
    </div>
  );
}

/** The server-derived cadence summary (or the validation error for a bad cron). */
function CadencePreviewLine({ preview }: { preview: CadencePreview | null }) {
  if (!preview) return null;
  if (preview.state === "loading") {
    return (
      <p className="flex items-center gap-1.5 text-[12px] text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" /> Checking schedule…
      </p>
    );
  }
  if (preview.state === "error") {
    return (
      <p className="text-[12px] text-destructive" role="alert">
        {preview.message}
      </p>
    );
  }
  return (
    <p className={cn("text-[12px] text-foreground")}>
      <span className="text-muted-foreground">Runs: </span>
      {preview.label}
    </p>
  );
}
