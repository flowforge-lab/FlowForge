import { useEffect, useState } from "react";
import {
  Check,
  CirclePlay,
  Loader2,
  Pause,
  Pencil,
  Play,
  Plus,
  SquareArrowOutUpRight,
  Trash2,
} from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useSettingsStore } from "@/store/settings";
import { useScheduledStore } from "@/store/scheduled";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import { ScheduledTaskForm } from "@/components/settings/scheduled-task-form";
import type { ScheduledTask } from "@/bindings";

const when = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  hour: "numeric",
  minute: "2-digit",
});

function fmt(ms: number | null | undefined): string {
  return ms == null ? "never" : when.format(ms);
}

/**
 * Scheduled section (#132 → #541 → #543): cron-scheduled agent tasks against the
 * real `ff-scheduled` commands. Lists tasks (built-in + user), with a new/edit form +
 * schedule builder and store-backed create / edit / delete / pause. Each row can be
 * fired now (▶) and, once a fire produces a session, jumped to (↗). `Next` / `Last`
 * live-update from `scheduled:fired` / `scheduled:changed`. Footer "Reset to defaults"
 * resumes every paused task.
 */
export function ScheduledSection() {
  const tasks = useScheduledStore((s) => s.tasks);
  const loading = useScheduledStore((s) => s.loading);
  const error = useScheduledStore((s) => s.error);
  const load = useScheduledStore((s) => s.load);
  const resetScheduled = useScheduledStore((s) => s.resetScheduled);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  // `"new"` while creating, a task id while editing that row, or null when closed.
  const [editing, setEditing] = useState<"new" | string | null>(null);

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
          onClick={() => setEditing((v) => (v === "new" ? null : "new"))}
        >
          <Plus />
          New task
        </Button>
      </div>

      {editing === "new" ? (
        <ScheduledTaskForm onDone={() => setEditing(null)} />
      ) : null}

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
          {tasks.map((task) =>
            editing === task.id ? (
              <li key={task.id}>
                <ScheduledTaskForm
                  task={task}
                  onDone={() => setEditing(null)}
                />
              </li>
            ) : (
              <TaskCard
                key={task.id}
                task={task}
                onEdit={() => setEditing(task.id)}
              />
            ),
          )}
        </ul>
      )}
    </div>
  );
}

function TaskCard({
  task,
  onEdit,
}: {
  task: ScheduledTask;
  onEdit: () => void;
}) {
  const toggle = useScheduledStore((s) => s.toggle);
  const remove = useScheduledStore((s) => s.remove);
  const runNow = useScheduledStore((s) => s.runNow);
  const saving = useScheduledStore((s) => s.saving);
  const running = useScheduledStore((s) => s.runningId === task.id);
  const sessionId = useScheduledStore((s) => s.runsByTask[task.id]);
  const isBuiltin = task.kind.kind === "builtin";

  // Leave Settings and land on the session the most recent fire produced. Load it
  // into the focused pane (so it shows where the user is looking), mirroring the
  // sidebar's open() (#148); fall back to a global switch before panes initialize.
  const openSession = () => {
    if (!sessionId) return;
    const chat = useChatStore.getState();
    const focused = usePanesStore.getState().focusedPaneId;
    if (focused) {
      usePanesStore.getState().setPaneSession(focused, sessionId);
      void chat.loadSession(sessionId);
    } else {
      void chat.selectSession(sessionId);
    }
    useSettingsStore.getState().closeSettings();
  };

  return (
    <li className="flex items-center gap-3 rounded-md border px-3 py-2.5">
      <span
        className={cn(
          "flex size-5 shrink-0 items-center justify-center rounded-full",
          task.paused
            ? "bg-muted text-muted-foreground"
            : "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
        )}
        title={task.paused ? "Paused" : "Running"}
        aria-label={task.paused ? "Paused" : "Running"}
      >
        {task.paused ? (
          <Pause className="size-3" />
        ) : (
          <Check className="size-3.5" />
        )}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-[13px] font-medium text-foreground">
            {task.name}
          </span>
          {isBuiltin ? (
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
          disabled={running || task.paused}
          onClick={() => void runNow(task.id)}
          title={
            task.paused ? "Resume to run" : running ? "Running…" : "Run now"
          }
          aria-label={running ? "Running task" : "Run task now"}
        >
          {running ? <Loader2 className="animate-spin" /> : <CirclePlay />}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground hover:text-foreground"
          disabled={!sessionId}
          onClick={openSession}
          title={sessionId ? "Open last run's session" : "No run yet"}
          aria-label={
            sessionId ? "Open last run's session" : "No run to open yet"
          }
        >
          <SquareArrowOutUpRight />
        </Button>
        {!isBuiltin ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground hover:text-foreground"
            onClick={onEdit}
            title="Edit task"
            aria-label="Edit task"
          >
            <Pencil />
          </Button>
        ) : null}
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
        {!isBuiltin ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground hover:text-destructive"
            disabled={saving}
            onClick={() => void remove(task.id)}
            title="Delete task"
            aria-label="Delete task"
          >
            <Trash2 />
          </Button>
        ) : null}
      </div>
    </li>
  );
}
