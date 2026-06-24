import { useEffect, useState } from "react";
import { ChevronRight, Download, Loader2 } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";
import { ToolStepBlock } from "@/components/tool-step";
import { ProseStepBlock } from "@/components/prose-step";
import { ThinkingBlock } from "@/components/thinking-block";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import {
  answerPreview,
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
  items,
  streaming,
  turnStartMs,
  reasoning,
  hasAnswer,
  answer,
  onExportTimeline,
  onRespond,
  onApproveSession,
  onApproveAlways,
  onAnswer,
}: {
  steps: ToolStep[];
  /** Ordered prose + step rows (#415). Defaults to the steps alone when omitted. */
  items?: TurnItem[];
  streaming: boolean;
  /** Wall-clock turn start from send / first stream (#180). */
  turnStartMs?: number | null;
  /** Model reasoning for this turn (#205); folds under this group, above the steps. */
  reasoning?: string;
  hasAnswer?: boolean;
  /** The turn's final answer text. When the group is collapsed, a muted 2-line
   *  preview of it shows under the header so the outcome is visible without
   *  expanding (#414). Raw content — no model-generated summary. */
  answer?: string;
  /** Dev-only step-timeline export (#417). When provided, a Download control shows in
   *  the header; gated upstream by the `stepTimelineExport` experimental flag. */
  onExportTimeline?: (format: "json" | "csv") => void;
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

  // Muted prose glimpse of the answer shown under the collapsed header (#414).
  const preview = answer ? answerPreview(answer) : "";

  const { visible } = selectStepWindow(steps, {
    streaming,
    awaiting,
    peekExpanded: effectivePeekExpanded,
  });

  // Render interleaved prose + steps (#415); fall back to the steps alone. While the
  // peek window hides earlier steps, drop the items before the first visible step so
  // the prose stays anchored to the steps it narrates.
  const allItems: TurnItem[] =
    items ?? steps.map((step) => ({ kind: "step", step }));
  const firstVisible = visible[0];
  const sliceFrom =
    visible.length < steps.length && firstVisible
      ? allItems.findIndex(
          (it) => it.kind === "step" && it.step === firstVisible,
        )
      : 0;
  const visibleItems = sliceFrom > 0 ? allItems.slice(sliceFrom) : allItems;

  return (
    <div className="w-full font-mono text-[11px]">
      {/* The fold toggle and the dev export control are siblings, so clicking the
          download icon doesn't toggle the fold. */}
      <div className="flex w-full items-center gap-1">
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setUserOpen(!open)}
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-2.5 py-1.5 text-left text-muted-foreground transition-colors hover:text-foreground"
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
        {onExportTimeline && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label="Export step timeline"
                title="Export step timeline (dev)"
                className="shrink-0 rounded-md p-1 text-muted-foreground/50 transition-colors hover:text-foreground"
              >
                <Download className="size-3.5" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => onExportTimeline("json")}>
                Download JSON
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => onExportTimeline("csv")}>
                Download CSV
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {!open && preview && (
        <p className="line-clamp-2 pb-1.5 pl-7 pr-2.5 font-sans leading-relaxed text-muted-foreground/70">
          {preview}
        </p>
      )}
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
          {visibleItems.map((it) =>
            it.kind === "prose" ? (
              <ProseStepBlock key={`prose:${it.key}`} text={it.text} />
            ) : (
              <ToolStepBlock
                key={it.step.callId}
                step={it.step}
                onRespond={onRespond}
                onApproveSession={onApproveSession}
                onApproveAlways={onApproveAlways}
                onAnswer={onAnswer}
              />
            ),
          )}
        </div>
      )}
    </div>
  );
}
