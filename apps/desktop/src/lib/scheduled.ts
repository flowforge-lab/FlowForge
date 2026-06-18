// FE-owned types for the Scheduled section (#132, SET.9). There is no backend/
// ts-rs type for scheduled agent tasks yet (the real cron runner is out of scope),
// so these live in `lib/` (mirroring SET.5's `lib/marketplace.ts`) and are
// `import type`'d into `ipc.ts`. `bindings/` stays untouched.

/** A cron-scheduled agent task as presented in Settings → Scheduled. */
export interface ScheduledTask {
  id: string;
  name: string;
  /** Built-in tasks ship with the app and can't be deleted (shown with a badge). */
  builtin: boolean;
  /** The raw cron expression the task will run on. */
  cron: string;
  /** Human cadence summary derived from `cron`, e.g. "Daily at 5:00 PM". */
  cadenceLabel: string;
  /** Next/last fire times as epoch ms; `null` when not yet scheduled / never run. */
  nextRun: number | null;
  lastRun: number | null;
  paused: boolean;
}

/** Payload for creating a task (the stub New-task form). */
export interface CreateScheduledTaskInput {
  name: string;
  cron: string;
  cadenceLabel: string;
}
