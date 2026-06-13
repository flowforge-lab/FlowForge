import { useState } from "react";
import { ChevronRight, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";
import { ToolStepBlock } from "@/components/tool-step";
import { formatDuration, groupDurationMs, resolveGroupOpen } from "@/lib/steps";

// Folds a turn's tool steps behind one "▸ N steps" header (Issue #17). Composes
// ToolStepBlock unchanged — this only adds the turn-level fold + duration. The
// fold state is local (mirrors tool-step.tsx's userToggled), extended to a
// tri-state because the default flips on turn:done: streaming → expanded,
// settled → collapsed, but a manual toggle wins and survives the flip.
export function StepGroup({
  steps,
  streaming,
  onRespond,
}: {
  steps: ToolStep[];
  streaming: boolean;
  onRespond: (callId: string, approved: boolean) => void;
}) {
  const awaiting = steps.some((s) => s.status === "awaiting-approval");
  // null = untouched (follow the turn); true/false = an explicit user choice.
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const open = resolveGroupOpen({ awaiting, userOpen, streaming });

  const durationMs = groupDurationMs(steps);
  const showDuration = !streaming && durationMs !== null;

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
        <span className="font-medium text-foreground">
          {steps.length} {steps.length === 1 ? "step" : "steps"}
        </span>
        {showDuration && (
          <span className="ml-auto tabular-nums text-muted-foreground/60">
            {formatDuration(durationMs)}
          </span>
        )}
      </button>
      {open && (
        <div className="mt-1.5 flex flex-col gap-1.5 border-l border-border/60 pl-2.5">
          {steps.map((step) => (
            <ToolStepBlock
              key={step.callId}
              step={step}
              onRespond={onRespond}
            />
          ))}
        </div>
      )}
    </div>
  );
}
