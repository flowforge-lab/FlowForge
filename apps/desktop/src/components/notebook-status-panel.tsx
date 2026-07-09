import { useEffect, useState } from "react";
import { ChevronRight, CircleDot, Square } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useNotebookStore } from "@/store/notebook";
import {
  NOTEBOOK_POLL_DEFAULT_MS,
  useExperimentalStore,
} from "@/store/experimental";

// Notebook kernel status panel (#871 FE-1). A self-hiding strip mounted above
// the goal panel inside each session pane. Three render states:
//
//   - never-polled (store entry undefined) -> render nothing yet — no flicker,
//     no error copy. The hydrate effect kicks off on mount.
//   - has-kernel=false (snapshot = null)   -> quiet "No kernel for this session"
//     line so a freshly-created session doesn't feel broken.
//   - has-kernel=true                      -> a state pill (live/dead) + the
//     kernel id + execution count, plus a Stop button while running.
//
// While `state == "running"` we poll `notebook_status` (cadence tunable from
// `useExperimentalStore.notebookPollIntervalMs`, default 5s) so the panel
// stays in sync with the cell execution counter. Pressing Stop removes the
// kernel outright (the real backend's `stop()` does `kernels.remove(...)`),
// collapsing the session to the "no kernel" row, not a `dead` tombstone —
// `dead` is reserved for a kernel that died on its own. Either way, leaving
// `running` stops the poll loop — no more IPC traffic until the next mount.
// A pushed `notebook:updated` event can replace polling later (tracked in
// #871).
//
// The panel is intentionally calm: no animation, no auto-expand. The chevron
// here only hides the (currently quiet) text block; the pill + Stop stay
// pinned on the header so a user always sees what's running.

export function NotebookStatusPanel({ sessionId }: { sessionId: string }) {
  const snapshot = useNotebookStore((s) => s.bySession[sessionId]);
  const hydrate = useNotebookStore((s) => s.hydrate);
  const refresh = useNotebookStore((s) => s.refresh);
  const stop = useNotebookStore((s) => s.stop);
  const pollMs = useExperimentalStore(
    (s) => s.notebookPollIntervalMs ?? NOTEBOOK_POLL_DEFAULT_MS,
  );

  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);

  // Hydrate on mount. The store handles IPC failure (leaves the entry
  // undefined) so a brief backend blip never strands the panel on an error
  // banner — the next mount retries.
  useEffect(() => {
    void hydrate(sessionId);
  }, [hydrate, sessionId]);

  // Poll while running. Cancels on unmount, on sessionId change, or when the
  // kernel leaves the `running` state — whether because it died on its own
  // (`dead`) or the user pressed Stop, which collapses the session straight
  // to "no kernel" (`snapshot` becomes `null`, so `hasKernel` is falsy and
  // this effect's guard bails the same way).
  useEffect(() => {
    if (!snapshot?.hasKernel || snapshot.state !== "running") return;
    let cancelled = false;
    const id = window.setInterval(() => {
      if (cancelled) return;
      void refresh(sessionId);
    }, pollMs);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [refresh, sessionId, snapshot?.hasKernel, snapshot?.state, pollMs]);

  if (snapshot === undefined) return null;
  if (snapshot === null || !snapshot.hasKernel) {
    return <NoKernelRow expanded={expanded} setExpanded={setExpanded} />;
  }

  const live = snapshot.state === "running";
  const dotTone = live ? "bg-emerald-500" : "bg-destructive";
  const label = live ? "kernel running" : "kernel dead";

  async function onStop() {
    setStopping(true);
    try {
      await stop(sessionId);
    } finally {
      setStopping(false);
    }
  }

  return (
    <div className="flex shrink-0 flex-col border-b bg-card/40">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        className="flex items-center gap-1.5 px-2.5 py-1.5 text-left transition-colors hover:bg-foreground/5"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            expanded && "rotate-90",
          )}
        />
        <span className={cn("size-1.5 shrink-0 rounded-full", dotTone)} />
        <span className="text-[11px] font-medium text-foreground">{label}</span>
        {snapshot.kernelId && (
          <span className="font-mono text-[11px] text-muted-foreground/70">
            {snapshot.kernelId}
          </span>
        )}
        <span className="text-[11px] text-muted-foreground/70">
          {snapshot.executionCount} cell
          {snapshot.executionCount === 1 ? "" : "s"} executed
        </span>
        {live && (
          <span className="ml-auto inline-flex items-center gap-1 text-[10px] text-muted-foreground/60">
            <Spinner className="size-3" />
            polling
          </span>
        )}
        {live && (
          <Button
            variant="outline"
            size="xs"
            className="ml-1 h-6 px-2 text-[11px]"
            disabled={stopping}
            onClick={(e) => {
              e.stopPropagation();
              void onStop();
            }}
            title="Stop the kernel"
          >
            <Square className="mr-1 size-3" />
            {stopping ? "Stopping…" : "Stop"}
          </Button>
        )}
      </button>
      {expanded && (
        <div className="space-y-1 border-t px-2.5 py-2 text-[11px] leading-relaxed text-muted-foreground/70">
          <p className="flex flex-wrap items-center gap-1.5">
            <CircleDot className="size-3" />
            <span>
              {snapshot.kernelId ?? "(no id)"}
              {snapshot.pid != null ? ` · pid ${snapshot.pid}` : ""}
            </span>
          </p>
          {snapshot.raw && (
            <p className="truncate font-mono text-[10px] text-muted-foreground/60">
              {snapshot.raw}
            </p>
          )}
          {!live && (
            <p className="text-[10px]">
              Kernel is dead. Ask the agent to call{" "}
              <code className="rounded bg-muted px-1 py-px font-mono">
                notebook_runner start
              </code>{" "}
              to spawn a new one.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

// Quiet row for the "no kernel" state. Self-renders the same height as the
// `live` row so the panel doesn't pop in/out as the kernel lifecycle turns.
function NoKernelRow({
  expanded,
  setExpanded,
}: {
  expanded: boolean;
  setExpanded: (v: boolean | ((s: boolean) => boolean)) => void;
}) {
  return (
    <div className="flex shrink-0 items-center border-b bg-card/30 px-2.5 py-1 text-[11px] text-muted-foreground/70">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        className="flex items-center gap-1.5 text-left"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            expanded && "rotate-90",
          )}
        />
        No kernel for this session
      </button>
    </div>
  );
}
