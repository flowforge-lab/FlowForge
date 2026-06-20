import { useEffect, useState } from "react";
import { ChevronRight, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";
import { ToolStepBlock } from "@/components/tool-step";
import { ThinkingBlock } from "@/components/thinking-block";
import {
  formatDuration,
  groupDurationMs,
  liveElapsedMs,
  resolveGroupOpen,
  selectStepWindow,
  STEP_WINDOW,
} from "@/lib/steps";

// Folds a turn's tool steps behind one "▸ N steps" header (Issue #17). Composes
// ToolStepBlock unchanged — this only adds the turn-level fold + duration. The
// fold state is local (mirrors tool-step.tsx's userToggled), extended to a
// tri-state because the default flips on turn:done: streaming → expanded,
// settled → collapsed, but a manual toggle wins and survives the flip.
export function StepGroup({
  steps,
  streaming,
  turnStartMs,
  reasoning,
  hasAnswer,
  onRespond,
  onApproveSession,
  onApproveAlways,
  onAnswer,
}: {
  steps: ToolStep[];
  streaming: boolean;
  /** Wall-clock turn start from send / first stream (#180). */
  turnStartMs?: number | null;
  /** Model reasoning for this turn (#205); folds under this group, above the steps. */
  reasoning?: string;
  hasAnswer?: boolean;
  onRespond: (callId: string, approved: boolean) => void;
  onApproveSession: (callId: string, tool: string) => void;
  onApproveAlways: (callId: string, tool: string) => void;
  onAnswer?: (callId: string, answer: string) => void;
}) {
  const awaiting = steps.some(
    (s) => s.status === "awaiting-approval" || s.status === "awaiting-answer",
  );
  // null = untouched (follow the turn); true/false = an explicit user choice.
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const [peekExpanded, setPeekExpanded] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const open = resolveGroupOpen({ awaiting, userOpen, streaming });
  const effectivePeekExpanded = streaming && peekExpanded;

  useEffect(() => {
    if (!streaming) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [streaming]);

  const durationMs = streaming
    ? liveElapsedMs(steps, now, turnStartMs)
    : groupDurationMs(steps);
  const showDuration = durationMs !== null;

  const earlierCount = Math.max(0, steps.length - STEP_WINDOW);

  const { visible } = selectStepWindow(steps, {
    streaming,
    awaiting,
    peekExpanded: effectivePeekExpanded,
  });

  return (
    <div className="w-full font-mono text-xs">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setUserOpen(!open)}
        className="flex w-full items-center gap-1.5 rounded-md px-2.5 py-1.5 text-left text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            open && "rotate-90",
          )}
        />
        {streaming && (
          <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
        )}
        <span className="min-w-0 flex-1 font-medium text-foreground">
          {steps.length} {steps.length === 1 ? "step" : "steps"}
          {showDuration && (
            <span className="font-normal text-muted-foreground/60">
              {" · "}
              {formatDuration(durationMs)}
            </span>
          )}
        </span>
      </button>
      {open && (
        <div className="mt-1.5 flex flex-col gap-1.5 border-l border-border/60 pl-2.5">
          {reasoning ? (
            <ThinkingBlock
              reasoning={reasoning}
              streaming={streaming}
              hasAnswer={hasAnswer ?? false}
            />
          ) : null}
          {earlierCount > 0 && streaming && !awaiting && (
            <button
              type="button"
              onClick={() => setPeekExpanded((v) => !v)}
              className="rounded-md px-2.5 py-1 text-left text-muted-foreground/70 transition-colors hover:text-foreground"
            >
              {effectivePeekExpanded
                ? `Show last ${STEP_WINDOW} steps`
                : `+${earlierCount} earlier ${earlierCount === 1 ? "step" : "steps"}`}
            </button>
          )}
          {visible.map((step) => (
            <ToolStepBlock
              key={step.callId}
              step={step}
              onRespond={onRespond}
              onApproveSession={onApproveSession}
              onApproveAlways={onApproveAlways}
              onAnswer={onAnswer}
            />
          ))}
        </div>
      )}
    </div>
  );
}
