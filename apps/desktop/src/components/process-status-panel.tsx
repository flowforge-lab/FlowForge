import { useEffect, useRef, useState } from "react";
import { Check, ChevronRight, Terminal, X } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Spinner } from "@/components/ui/spinner";
import { useProcessesStore } from "@/store/processes";
import type { ProcessState } from "@/store/processes";

// Live background-process output (#873 FE / #987). A self-hiding strip mounted
// above the notebook/goal panels inside each session pane. It renders one row
// per background process started via `process_manager` in this session, each
// with its live stdout/stderr tail and a terminal status badge once the process
// exits.
//
// Unlike tool output (turn-scoped, keyed by messageId), a background process
// keeps streaming across turns for its whole life — the store (`processes.ts`)
// is a session-scoped, cross-turn sink pushed to by the `process:output` /
// `process:exited` events wired in `lib/events.ts`. This panel just reflects it.
//
// Self-hides when the session has no background processes (store entry absent or
// empty), so a session that never starts one shows nothing at all.

export function ProcessStatusPanel({ sessionId }: { sessionId: string }) {
  const byId = useProcessesStore((s) => s.bySession[sessionId]);

  if (!byId) return null;
  // Newest first — a dev server started this turn should sit at the top.
  const processes = Object.values(byId).sort(
    (a, b) => b.processId - a.processId,
  );
  if (processes.length === 0) return null;

  return (
    <div className="flex shrink-0 flex-col border-b bg-card/40">
      {processes.map((p) => (
        <ProcessRow key={p.processId} process={p} />
      ))}
    </div>
  );
}

// One process's header + collapsible output. Running processes default to
// expanded (the user just started them and wants to watch); exited ones default
// to collapsed (the run is over, keep the strip compact) — but either can be
// toggled.
function ProcessRow({ process }: { process: ProcessState }) {
  const running = process.status === null;
  const [expanded, setExpanded] = useState(running);

  const kind = classify(process.status);
  const dotTone =
    kind === "running"
      ? "bg-emerald-500"
      : kind === "ok"
        ? "bg-emerald-500"
        : "bg-destructive";

  return (
    <div className="flex flex-col border-b last:border-b-0">
      <div className="flex items-center gap-1.5 px-2.5 py-1.5">
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
          className="flex flex-1 items-center gap-1.5 text-left transition-colors hover:bg-foreground/5"
        >
          <ChevronRight
            className={cn(
              "size-3.5 shrink-0 text-muted-foreground transition-transform",
              expanded && "rotate-90",
            )}
          />
          <Terminal className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="text-[11px] font-medium text-foreground">
            Process #{process.processId}
          </span>
          <span className={cn("size-1.5 shrink-0 rounded-full", dotTone)} />
          <ProcessStatusBadge status={process.status} />
        </button>
      </div>
      {expanded && <ProcessOutput output={process.output} />}
    </div>
  );
}

// A small running/ok/error badge, mirroring the notebook cell status badge. The
// terminal `status` label from the backend (`"exited(0)"`, `"killed"`,
// `"failed: <reason>"`) is shown verbatim so the user sees the real reason.
function ProcessStatusBadge({ status }: { status: string | null }) {
  const kind = classify(status);
  if (kind === "running") {
    return (
      <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground">
        <Spinner className="size-3" />
        running
      </span>
    );
  }
  if (kind === "ok") {
    return (
      <span className="inline-flex items-center gap-1 text-[11px] text-emerald-600 dark:text-emerald-400">
        <Check className="size-3" />
        {status}
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-[11px] text-destructive">
      <X className="size-3" />
      {status}
    </span>
  );
}

// Append-only, autoscrolling monospace output. Sticks to the bottom while new
// chunks arrive, but respects a manual scroll-up (the same sticky-bottom logic
// the chat transcript uses, `chat-view.tsx`). No fold cap: a live log is the
// whole point, so it scrolls inside a bounded box rather than folding.
function ProcessOutput({ output }: { output: string }) {
  const scrollRef = useRef<HTMLPreElement>(null);
  const pinnedToBottom = useRef(true);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedToBottom.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [output]);

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    pinnedToBottom.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  }

  return (
    <pre
      ref={scrollRef}
      onScroll={handleScroll}
      data-process-output
      className="max-h-48 overflow-auto whitespace-pre-wrap break-words border-t bg-card/60 px-2.5 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground"
    >
      {output || " "}
    </pre>
  );
}

type StatusKind = "running" | "ok" | "error";

// A process is running until it exits; `"exited(0)"` is the only success label,
// everything else (`"killed"`, `"failed: …"`, non-zero exit) is an error.
function classify(status: string | null): StatusKind {
  if (status === null) return "running";
  return status === "exited(0)" ? "ok" : "error";
}
