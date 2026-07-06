import { useEffect, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  CircleDot,
  CornerDownLeft,
  Pause,
  Play,
  Square,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { formatDuration } from "@/lib/steps";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { useGoalStore } from "@/store/goal";
import { useChatStore } from "@/store/chat";
import type { GoalStatus } from "@/bindings/GoalStatus";

// Goal status panel (#717, RFC 0020). A calm, self-hiding strip that sits above a
// pane's transcript whenever that session has a goal. It renders the objective,
// live budget gauges, the last ledger action, and pause / abort / steer controls,
// all driven off the `goal:updated` event (wired in lib/events.ts) — no polling.
// Scoped to one session because a goal is single-session (RFC 0020 §3).

function formatTokens(n: number): string {
  if (n < 1_000) return String(n);
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

const STATUS_META: Record<
  GoalStatus,
  {
    label: string;
    tone: "neutral" | "amber" | "emerald" | "sky" | "destructive";
  }
> = {
  active: { label: "Active", tone: "sky" },
  paused: { label: "Paused", tone: "amber" },
  completed: { label: "Completed", tone: "emerald" },
  exhausted: { label: "Exhausted", tone: "amber" },
  failed: { label: "Failed", tone: "destructive" },
};

// One budget dimension. `max == null` renders the raw count without a bar (that
// dimension is unbounded — RFC 0020 GoalBudget optionals).
function Gauge({
  label,
  value,
  max,
  format,
}: {
  label: string;
  value: number;
  max?: number | null;
  format: (n: number) => string;
}) {
  const pct = max != null && max > 0 ? (value / max) * 100 : null;
  const near = pct != null && pct >= 80;
  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-[11px] font-medium text-muted-foreground">
          {label}
        </span>
        <span
          className={cn(
            "text-[11px] tabular-nums",
            near ? "text-amber-600 dark:text-amber-400" : "text-foreground",
          )}
        >
          {format(value)}
          {max != null ? ` / ${format(max)}` : ""}
        </span>
      </div>
      {pct != null && (
        <Progress value={pct} className="mt-1 h-1.5" aria-label={label} />
      )}
    </div>
  );
}

export function GoalStatusPanel({ sessionId }: { sessionId: string }) {
  const goal = useGoalStore((s) => s.bySession[sessionId]);
  const hydrate = useGoalStore((s) => s.hydrate);
  const pause = useGoalStore((s) => s.pause);
  const resume = useGoalStore((s) => s.resume);
  const abort = useGoalStore((s) => s.abort);
  const send = useChatStore((s) => s.send);

  const [expanded, setExpanded] = useState(true);
  const [confirmAbort, setConfirmAbort] = useState(false);
  const [steer, setSteer] = useState("");

  // Close the race where a goal already exists before the event listener attached.
  useEffect(() => {
    void hydrate(sessionId);
  }, [sessionId, hydrate]);

  if (!goal) return null;

  const terminal =
    goal.status === "completed" ||
    goal.status === "exhausted" ||
    goal.status === "failed";
  const last = goal.ledger[goal.ledger.length - 1];
  // RFC 0020 §4 safety pause: an `Ask` cell hit mid-iteration pauses the loop with
  // the active step's `next: ask_user`. That is a "needs your input" state, visually
  // distinct from a manual Pause — otherwise the user can't tell the loop is blocked
  // on them. (The real loop sets this in Track F; the mock exercises it via "[ask]".)
  const isAskPause = goal.status === "paused" && last?.next === "ask_user";
  const meta = isAskPause
    ? { label: "Needs review", tone: "amber" as const }
    : STATUS_META[goal.status];
  // Disclosure a11y: id the collapsible body and point the toggle at it.
  const bodyId = `goal-body-${sessionId}`;

  const submitSteer = () => {
    const text = steer.trim();
    if (!text) return;
    void send(text, sessionId);
    setSteer("");
  };

  return (
    <div className="shrink-0 border-b bg-card/40">
      <div className="flex items-center gap-2 px-3 py-1.5">
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
          aria-expanded={expanded}
          aria-controls={bodyId}
          aria-label={expanded ? "Collapse goal" : "Expand goal"}
        >
          {expanded ? (
            <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
          )}
          <CircleDot className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 truncate text-xs font-medium text-foreground">
            {goal.objective}
          </span>
        </button>
        <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
          iter {goal.iteration}/{goal.budget.maxIterations}
        </span>
        <Badge tone={meta.tone} className="gap-1">
          {isAskPause && <AlertTriangle className="size-3" aria-hidden />}
          {meta.label}
        </Badge>
      </div>

      {expanded && (
        <div id={bodyId} className="space-y-2.5 px-3 pb-2.5">
          {isAskPause && (
            <div className="flex items-start gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-[11px] text-amber-700 dark:text-amber-400">
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              <span>
                Needs review — the goal paused for your input. Resume to
                continue once you&apos;ve responded.
              </span>
            </div>
          )}
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            <Gauge
              label="Iterations"
              value={goal.iteration}
              max={goal.budget.maxIterations}
              format={(n) => String(n)}
            />
            <Gauge
              label="Tokens"
              value={goal.spent.tokens}
              max={goal.budget.maxTokens}
              format={formatTokens}
            />
            <Gauge
              label="Wall"
              value={goal.spent.wallMs}
              max={goal.budget.maxWallMs}
              format={formatDuration}
            />
          </div>

          {last && (
            <div className="min-w-0 text-[11px] text-muted-foreground">
              <span className="font-medium text-foreground">Last action: </span>
              <span className="break-words">{last.action ?? last.claim}</span>
              {last.verdict && (
                <span className="ml-1 uppercase tracking-wide opacity-70">
                  · {last.verdict}
                </span>
              )}
            </div>
          )}

          {goal.pendingSteer && (
            <div className="truncate text-[11px] text-sky-700 dark:text-sky-400">
              Steer queued: {goal.pendingSteer}
            </div>
          )}

          <div className="flex flex-wrap items-center gap-1.5">
            {goal.status === "active" && (
              <Button
                size="xs"
                variant="secondary"
                onClick={() => void pause(sessionId)}
              >
                <Pause className="size-3" />
                Pause
              </Button>
            )}
            {goal.status === "paused" && (
              <Button
                size="xs"
                variant="secondary"
                onClick={() => void resume(sessionId)}
              >
                <Play className="size-3" />
                Resume
              </Button>
            )}

            {terminal ? (
              <Button
                size="xs"
                variant="ghost"
                onClick={() => void abort(sessionId)}
              >
                Dismiss
              </Button>
            ) : confirmAbort ? (
              <>
                <Button
                  size="xs"
                  variant="destructive"
                  onClick={() => void abort(sessionId)}
                >
                  Confirm abort
                </Button>
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => setConfirmAbort(false)}
                >
                  Cancel
                </Button>
              </>
            ) : (
              <Button
                size="xs"
                variant="ghost"
                className="text-destructive"
                onClick={() => setConfirmAbort(true)}
              >
                <Square className="size-3" />
                Abort
              </Button>
            )}
          </div>

          {goal.status === "active" && (
            <div className="flex items-center gap-1.5">
              <Input
                value={steer}
                onChange={(e) => setSteer(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    submitSteer();
                  }
                }}
                placeholder="Steer the goal…"
                className="h-7 text-xs"
                aria-label="Steer the goal"
              />
              <Button
                size="icon-xs"
                variant="ghost"
                disabled={!steer.trim()}
                onClick={submitSteer}
                title="Send steer"
                aria-label="Send steer"
              >
                <CornerDownLeft className="size-3.5" />
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
