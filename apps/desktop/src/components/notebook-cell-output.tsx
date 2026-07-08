import { Check, Loader2, X } from "@/components/ui/icon";
import { Badge } from "@/components/ui/badge";
import { HighlightedCode } from "@/components/markdown";
import {
  parseNotebookStep,
  parseKernelStatus,
  type NotebookStep,
} from "@/lib/notebook-output";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";

/**
 * Notebook-styled cell renderer for the `notebook_runner` tool (#871 FE-2).
 *
 * Lays out a tool step as a notebook cell: an optional `code` block (for
 * `run_cell`), the textual output stream, and an ok/error badge. The
 * `status` action gets a small kernel-state pill derived from the canonical
 * `kernel <id> — <state>; pid=…; cells executed=…` line the backend emits.
 *
 * This renderer is intentionally tolerant of the current Phase 1/2 text
 * contract — the parser (`lib/notebook-output.ts`) already returns a stable
 * shape, and the optional `images` / `variables` blocks below are gated on
 * presence so Phase 3's structured payload can plug in without changing the
 * surrounding layout.
 */
export function NotebookCellOutput({ step }: { step: ToolStep }) {
  const parsed = parseNotebookStep(
    step.tool,
    step.args,
    step.result,
    step.status === "error"
      ? "error"
      : step.status === "done"
        ? "done"
        : "running",
  );
  if (!parsed) return null;
  return (
    <NotebookCellBody
      step={parsed}
      live={step.output}
      running={step.status === "running"}
    />
  );
}

function NotebookCellBody({
  step,
  live,
  running,
}: {
  step: NotebookStep;
  /** Live `tool:output` stream accumulated while the cell runs. */
  live: string | undefined;
  running: boolean;
}) {
  // While running we mirror the live stream so slow cells visibly progress
  // (#680) — same convention as the generic OutputBlock. On completion
  // `step.result` (the parsed `output` above) supersedes it.
  const textWhileRunning =
    running && live !== undefined && live.length > 0 ? live : null;

  return (
    <div className="space-y-2">
      {step.code !== null && (
        <NotebookSection label="code">
          <HighlightedCode lang="python" text={step.code} />
        </NotebookSection>
      )}

      {step.action === "status" ? (
        <StatusLine raw={textWhileRunning ?? step.output} />
      ) : (
        <NotebookSection
          label={step.action === "run_cell" ? "output" : "result"}
          error={step.errored}
          trailing={
            step.action === "run_cell" ? (
              <CellStatusBadge
                errored={step.errored}
                running={running && textWhileRunning !== null}
                truncated={step.parsedExceptionTrailer}
              />
            ) : null
          }
        >
          {textWhileRunning !== null ? textWhileRunning : step.output}
        </NotebookSection>
      )}

      {/*
        Phase 3 forward-compat:
        - images:  images?.[i].dataUrl (a `data:` URI; we never set raw html src)
        - variables:  table derived from the structured variable dump
        The shape lands with the Phase 3 backend (FE-0 contract from the issue);
        we add the rendering here only when `parsed` carries the new fields.
       */}
    </div>
  );
}

function NotebookSection({
  label,
  error,
  trailing,
  children,
}: {
  label: string;
  error?: boolean;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-0.5 flex items-center justify-between gap-2">
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
          {label}
        </span>
        {trailing}
      </div>
      <pre
        className={cn(
          "max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed",
          error ? "text-destructive" : "text-foreground/90",
        )}
      >
        {children}
      </pre>
    </div>
  );
}

function CellStatusBadge({
  errored,
  running,
  truncated,
}: {
  errored: boolean;
  running: boolean;
  truncated: boolean;
}) {
  if (running) {
    return (
      <Badge tone="sky">
        <Loader2 className="mr-1 size-3 animate-spin" />
        running
      </Badge>
    );
  }
  if (errored) {
    return (
      <Badge tone="destructive">
        <X className="mr-1 size-3" />
        {truncated ? "exception" : "error"}
      </Badge>
    );
  }
  return (
    <Badge tone="emerald">
      <Check className="mr-1 size-3" />
      ok
    </Badge>
  );
}

function StatusLine({ raw }: { raw: string }) {
  const status = parseKernelStatus(raw);
  // No kernel: a quiet dot + label, no error copy. The StatusIcon in the
  // step header already shows a green check on a successful `status` call,
  // so we keep this minimal to avoid duplicating the affordance.
  if (status.state === "no-kernel") {
    return (
      <p className="text-[11px] text-muted-foreground/70">
        no kernel for this session
      </p>
    );
  }
  // Unknown / unparseable line: fall back to the raw text so the user can
  // always read what the backend reported.
  if (status.state === "unknown") {
    return (
      <p className="font-mono text-[11px] leading-relaxed text-foreground/80">
        {raw.trim() || "(no output)"}
      </p>
    );
  }
  const dotTone = status.state === "live" ? "bg-emerald-500" : "bg-destructive";
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
      <span className={cn("size-1.5 shrink-0 rounded-full", dotTone)} />
      <span className="font-medium text-foreground">
        {status.state === "live" ? "kernel running" : "kernel dead"}
      </span>
      {status.kernelId && (
        <span className="font-mono text-muted-foreground/70">
          {status.kernelId}
        </span>
      )}
      {status.executionCount !== null && (
        <span className="text-muted-foreground/70">
          {status.executionCount} cell{status.executionCount === 1 ? "" : "s"}{" "}
          executed
        </span>
      )}
    </div>
  );
}
