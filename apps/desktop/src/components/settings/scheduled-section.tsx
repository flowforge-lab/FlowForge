import { useEffect, useState } from "react";
import { Pause, Play, Plus, SquareArrowOutUpRight } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { useSettingsStore } from "@/store/settings";
import { useScheduledStore } from "@/store/scheduled";
import type { ScheduledTask } from "@/lib/scheduled";

const when = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  hour: "numeric",
  minute: "2-digit",
});

function fmt(ms: number | null): string {
  return ms === null ? "never" : when.format(ms);
}

/**
 * Scheduled section (#132, SET.9): cron-scheduled agent tasks. Lists tasks from
 * the mock IPC (a built-in "Memory Organizer" + a user task), supports pause/resume
 * and a stub New-task form. Footer "Reset to defaults" resumes every paused task.
 */
export function ScheduledSection() {
  const tasks = useScheduledStore((s) => s.tasks);
  const loading = useScheduledStore((s) => s.loading);
  const error = useScheduledStore((s) => s.error);
  const load = useScheduledStore((s) => s.load);
  const resetScheduled = useScheduledStore((s) => s.resetScheduled);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  const [creating, setCreating] = useState(false);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    registerResetHandler(() => void resetScheduled());
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetScheduled]);

  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-4">
        <p className="text-[12px] leading-relaxed text-muted-foreground">
          Run agent tasks on a schedule — built-in maintenance jobs and your own
          recurring prompts.
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="shrink-0"
          onClick={() => setCreating((v) => !v)}
        >
          <Plus />
          New task
        </Button>
      </div>

      {creating ? <NewTaskForm onDone={() => setCreating(false)} /> : null}

      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}

      {loading && tasks.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">Loading tasks…</p>
      ) : tasks.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">
          No scheduled tasks yet.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {tasks.map((task) => (
            <TaskCard key={task.id} task={task} />
          ))}
        </ul>
      )}
    </div>
  );
}

function TaskCard({ task }: { task: ScheduledTask }) {
  const toggle = useScheduledStore((s) => s.toggle);
  const saving = useScheduledStore((s) => s.saving);

  return (
    <li className="flex items-center gap-3 rounded-md border px-3 py-2.5">
      <span
        className={cn(
          "size-2 shrink-0 rounded-full",
          task.paused ? "bg-muted-foreground/40" : "bg-emerald-500",
        )}
        aria-label={task.paused ? "Paused" : "Running"}
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-[13px] font-medium text-foreground">
            {task.name}
          </span>
          {task.builtin ? (
            <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-muted-foreground">
              Builtin
            </span>
          ) : null}
        </div>
        <p className="text-[11px] text-muted-foreground">{task.cadenceLabel}</p>
        <p className="text-[10px] text-muted-foreground">
          Next {fmt(task.nextRun)} · Last {fmt(task.lastRun)}
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground hover:text-foreground"
          disabled
          title="Open task — coming soon"
          aria-label="Open task — coming soon"
        >
          <SquareArrowOutUpRight />
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground hover:text-foreground"
          disabled={saving}
          onClick={() => void toggle(task.id)}
          title={task.paused ? "Resume" : "Pause"}
          aria-label={task.paused ? "Resume task" : "Pause task"}
        >
          {task.paused ? <Play /> : <Pause />}
        </Button>
      </div>
    </li>
  );
}

/** Stub New-task form: name + cadence label → create. cron is a placeholder until
 *  the real scheduler lands. */
function NewTaskForm({ onDone }: { onDone: () => void }) {
  const create = useScheduledStore((s) => s.create);
  const saving = useScheduledStore((s) => s.saving);
  const [name, setName] = useState("");
  const [cadence, setCadence] = useState("");

  const canCreate = name.trim() !== "" && cadence.trim() !== "";

  const submit = () => {
    if (!canCreate) return;
    void create({
      name: name.trim(),
      cron: "0 9 * * *", // placeholder; real cron editing is out of scope
      cadenceLabel: cadence.trim(),
    }).then(onDone);
  };

  return (
    <div className="space-y-3 rounded-md border bg-muted/30 p-3">
      <div className="space-y-1.5">
        <label
          htmlFor="task-name"
          className="text-[11px] font-medium text-muted-foreground"
        >
          Name
        </label>
        <Input
          id="task-name"
          value={name}
          placeholder="Weekly Digest"
          autoComplete="off"
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="space-y-1.5">
        <label
          htmlFor="task-cadence"
          className="text-[11px] font-medium text-muted-foreground"
        >
          Cadence
        </label>
        <Input
          id="task-cadence"
          value={cadence}
          placeholder="Mondays at 9:00 AM"
          autoComplete="off"
          onChange={(e) => setCadence(e.target.value)}
        />
      </div>
      <div className="flex items-center justify-end gap-1.5">
        <Button type="button" variant="ghost" size="sm" onClick={onDone}>
          Cancel
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canCreate || saving}
          onClick={submit}
        >
          Create
        </Button>
      </div>
    </div>
  );
}
