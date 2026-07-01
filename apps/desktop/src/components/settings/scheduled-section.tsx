import { useEffect, useState } from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
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
import { SettingsSwitch } from "@/components/settings/switch";
import { useSettingsStore } from "@/store/settings";
import { useScheduledStore } from "@/store/scheduled";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import { ScheduledTaskForm } from "@/components/settings/scheduled-task-form";
import type { RunRecord, RunStatus, ScheduledTask } from "@/bindings";

const when = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  hour: "numeric",
  minute: "2-digit",
});

function fmt(ms: number | null | undefined): string {
  return ms == null ? "never" : when.format(ms);
}

const whenFull = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** Label + dot colour for a run's terminal status (RFC 0017 §8.4). */
const RUN_STATUS: Record<RunStatus, { label: string; dot: string }> = {
  ok: { label: "Ok", dot: "bg-emerald-500" },
  error: { label: "Error", dot: "bg-destructive" },
  cancelled: { label: "Cancelled", dot: "bg-muted-foreground" },
  needs_attention: { label: "Needs attention", dot: "bg-amber-500" },
};

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
  const pausedAll = useScheduledStore((s) => s.pausedAll);
  const setPausedAll = useScheduledStore((s) => s.setPausedAll);
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

      <div
        className={cn(
          "rounded-md border px-3 py-2.5",
          pausedAll && "border-amber-500/40 bg-amber-500/5",
        )}
      >
        <SettingsSwitch
          label="Pause all scheduled tasks"
          description="A global kill-switch — while on, nothing fires (including manual runs and tasks added later), regardless of each task's own state."
          checked={pausedAll}
          onCheckedChange={(v) => void setPausedAll(v)}
        />
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
  const loadRuns = useScheduledStore((s) => s.loadRuns);
  const saving = useScheduledStore((s) => s.saving);
  const running = useScheduledStore((s) => s.runningId === task.id);
  const pausedAll = useScheduledStore((s) => s.pausedAll);
  const sessionId = useScheduledStore((s) => s.runsByTask[task.id]);
  const history = useScheduledStore((s) => s.historyByTask[task.id]);
  const loadingRuns = useScheduledStore((s) => s.loadingRunsIds.has(task.id));
  const isBuiltin = task.kind.kind === "builtin";

  const [expanded, setExpanded] = useState(false);

  // Fetch history when the panel opens; refresh on each open so a row re-expanded
  // after new fires shows them even if the live event was missed. The fetch runs
  // outside the state updater (which must stay pure — `loadRuns` writes the store).
  const toggleHistory = () => {
    const next = !expanded;
    setExpanded(next);
    if (next) void loadRuns(task.id);
  };

  // Leave Settings and land on a fire's session. Load it into the focused pane (so
  // it shows where the user is looking), mirroring the sidebar's open() (#148); fall
  // back to a global switch before panes initialize.
  const openSession = (sid: string) => {
    const chat = useChatStore.getState();
    const focused = usePanesStore.getState().focusedPaneId;
    if (focused) {
      usePanesStore.getState().setPaneSession(focused, sid);
      void chat.loadSession(sid);
    } else {
      void chat.selectSession(sid);
    }
    useSettingsStore.getState().closeSettings();
  };

  return (
    <li className="rounded-md border">
      <div className="flex items-center gap-3 px-3 py-2.5">
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
          <p className="text-[11px] text-muted-foreground">
            {task.cadenceLabel}
          </p>
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
            onClick={toggleHistory}
            title={expanded ? "Hide run history" : "Show run history"}
            aria-label={expanded ? "Hide run history" : "Show run history"}
            aria-expanded={expanded}
          >
            {expanded ? <ChevronDown /> : <ChevronRight />}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground hover:text-foreground"
            disabled={running || task.paused || pausedAll}
            onClick={() => void runNow(task.id)}
            title={
              pausedAll
                ? "All tasks are paused"
                : task.paused
                  ? "Resume to run"
                  : running
                    ? "Running…"
                    : "Run now"
            }
            aria-label={
              pausedAll
                ? "Run task now (all tasks paused)"
                : running
                  ? "Running task"
                  : "Run task now"
            }
          >
            {running ? <Loader2 className="animate-spin" /> : <CirclePlay />}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground hover:text-foreground"
            disabled={!sessionId}
            onClick={() => sessionId && openSession(sessionId)}
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
      </div>
      {expanded ? (
        <RunHistory
          records={history}
          loading={loadingRuns}
          onOpen={openSession}
        />
      ) : null}
    </li>
  );
}

/** Per-task fire history (newest first): status, time, and a ↗ to the run's
 *  session when it created one. Renders loading / empty states explicitly. */
function RunHistory({
  records,
  loading,
  onOpen,
}: {
  records: RunRecord[] | undefined;
  loading: boolean;
  onOpen: (sessionId: string) => void;
}) {
  return (
    <div className="border-t px-3 py-2">
      {loading && records === undefined ? (
        <p className="text-[11px] text-muted-foreground">Loading history…</p>
      ) : !records || records.length === 0 ? (
        <p className="text-[11px] text-muted-foreground">No runs yet.</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {records.map((run) => {
            const status = RUN_STATUS[run.status];
            return (
              <li key={run.id} className="flex items-center gap-2 text-[11px]">
                <span
                  className={cn("size-1.5 shrink-0 rounded-full", status.dot)}
                  aria-hidden
                />
                <span className="w-24 shrink-0 text-muted-foreground">
                  {status.label}
                </span>
                <span className="flex-1 truncate text-muted-foreground">
                  {whenFull.format(run.firedMs)}
                </span>
                {run.sessionId ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="text-muted-foreground hover:text-foreground"
                    onClick={() => run.sessionId && onOpen(run.sessionId)}
                    title="Open this run's session"
                    aria-label="Open this run's session"
                  >
                    <SquareArrowOutUpRight />
                  </Button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
