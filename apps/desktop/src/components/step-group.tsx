import { useEffect, useState } from "react";
import { ChevronRight, Download } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Spinner } from "@/components/ui/spinner";
import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";
import { ToolStepBlock } from "@/components/tool-step";
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
  selectItemWindow,
  STEP_WINDOW,
} from "@/lib/steps";

// A short/operational narration line folded inside the step group (#687). Mirrors
// ThinkingBlock's compact row: a muted, collapsible line labelled "Thought" with a
// truncated preview, so throwaway narration stays scannable without the visual weight
// of a full-width prose block. Collapsed by default; a manual toggle reveals the full
// text.
function ThoughtRow({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const trimmed = text.trim();
  return (
    <div className="w-full font-mono text-[11px]">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 rounded-md px-2.5 py-1 text-left text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            open && "rotate-90",
          )}
        />
        <span className="shrink-0 font-medium text-foreground/90">Thought</span>
        {!open && (
          <span className="min-w-0 truncate text-muted-foreground/70">
            {trimmed.slice(0, 120)}
            {trimmed.length > 120 ? "…" : ""}
          </span>
        )}
      </button>
      {open && (
        <p
          data-selectable
          className="whitespace-pre-wrap px-2.5 pb-1 pl-7 font-sans leading-relaxed text-muted-foreground"
        >
          {trimmed}
        </p>
      )}
    </div>
  );
}

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
  hasAnswer,
  answer,
  onExportTimeline,
  onRespond,
  onApproveSession,
  onApproveAlways,
  onAnswer,
}: {
  steps: ToolStep[];
  /** Ordered reasoning + step rows for this segment (#574/#619). Each iteration's
   *  reasoning is a `reasoning` item in position; defaults to the steps alone when
   *  omitted. Intermediate prose is hoisted to a top-level block by `segmentTurn`, so
   *  it never reaches this group. */
  items?: TurnItem[];
  streaming: boolean;
  /** Wall-clock turn start from send / first stream (#180). */
  turnStartMs?: number | null;
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

  // Muted prose glimpse of the answer shown under the collapsed header (#414).
  const preview = answer ? answerPreview(answer) : "";

  // Render interleaved reasoning + thoughts + steps (#415/#574/#687); fall back to the
  // steps alone. While streaming, window to the last STEP_WINDOW *items* (steps and
  // folded thoughts alike, #687) so the live view stays to recent activity; the peek
  // ("+N earlier steps") expands to the full list, and settling shows everything.
  const allItems: TurnItem[] =
    items ?? steps.map((step) => ({ kind: "step", step }));
  const { visible: visibleItems, hiddenCount: earlierCount } = selectItemWindow(
    allItems,
    {
      streaming,
      awaiting,
      peekExpanded: effectivePeekExpanded,
    },
  );

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
          {streaming && <Spinner className="shrink-0 text-muted-foreground" />}
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
          {earlierCount > 0 && streaming && !awaiting && (
            <button
              type="button"
              onClick={() => setPeekExpanded((v) => !v)}
              className="rounded-md px-2.5 py-1 text-left text-muted-foreground/70 transition-colors hover:text-foreground"
            >
              {/* Noun-neutral: `earlierCount` counts items (steps + folded thoughts,
                  #687), so a fixed "steps" noun could misread when a thought is hidden. */}
              {effectivePeekExpanded
                ? `Show last ${STEP_WINDOW}`
                : `+${earlierCount} earlier`}
            </button>
          )}
          {visibleItems.map((it) =>
            it.kind === "reasoning" ? (
              <ThinkingBlock
                key={`reasoning:${it.key}`}
                reasoning={it.text}
                streaming={streaming}
                hasAnswer={hasAnswer ?? false}
              />
            ) : it.kind === "step" ? (
              <ToolStepBlock
                key={it.step.callId}
                step={it.step}
                onRespond={onRespond}
                onApproveSession={onApproveSession}
                onApproveAlways={onApproveAlways}
                onAnswer={onAnswer}
              />
            ) : it.kind === "thought" ? (
              // Short/operational narration folded inside the group (#687).
              <ThoughtRow key={`thought:${it.key}`} text={it.text} />
            ) : // Substantive intermediate prose is hoisted to a top-level block by
            // `segmentTurn` (#619/#687) and never reaches this group; the branch stays
            // for exhaustiveness.
            null,
          )}
        </div>
      )}
    </div>
  );
}
