import { useEffect, useState } from "react";
import {
  ChevronRight,
  CircleDot,
  RotateCcw,
  Square,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useNotebookStore } from "@/store/notebook";
import type { KernelInfo } from "@/store/notebook";
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
//
// Multi-kernel (#871 FE-2): a session may hold up to 3 kernels. When more than
// one is live the header grows a `KernelTabs` row; the selected tab is the
// "active" kernel every control (pill, Stop, Restart) acts on. With ≤1 kernel
// the panel is byte-for-byte the FE-1 single-kernel view. The switcher reads
// the structured `kernels[]` on `NotebookKernelState` (ts-rs binding, #924) —
// present for any live session, one entry per kernel; tabs show only when
// there's more than one.

export function NotebookStatusPanel({ sessionId }: { sessionId: string }) {
  const snapshot = useNotebookStore((s) => s.bySession[sessionId]);
  const hydrate = useNotebookStore((s) => s.hydrate);
  const refresh = useNotebookStore((s) => s.refresh);
  const stop = useNotebookStore((s) => s.stop);
  const restart = useNotebookStore((s) => s.restart);
  const pollMs = useExperimentalStore(
    (s) => s.notebookPollIntervalMs ?? NOTEBOOK_POLL_DEFAULT_MS,
  );

  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);
  const [restarting, setRestarting] = useState(false);
  // Restart discards the kernel's in-memory state (globals, execution count), so
  // it goes behind a confirm — unlike Stop, which just ends a process the user
  // already meant to end.
  const [confirmRestart, setConfirmRestart] = useState(false);
  // The tab the user picked in a multi-kernel session; `null` (or a stale id)
  // falls back to the representative kernel below.
  const [selectedKernelId, setSelectedKernelId] = useState<string | null>(null);

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

  // Multi-kernel enumeration (#871 FE-2), present only when a session holds >1
  // kernel. `null` → the single-kernel view (identical to FE-1).
  const kernels = snapshot.kernels ?? null;
  const multi = kernels !== null && kernels.length > 1;
  // The kernel every control acts on: the selected tab in multi mode (falling
  // back to the representative if the selection is stale/absent), else the
  // snapshot's representative fields.
  const active = multi
    ? (kernels.find((k) => k.kernelId === selectedKernelId) ??
      // No (or stale) selection: default to the representative kernel — the
      // live one the snapshot already surfaces — not just the first tab.
      kernels.find((k) => k.kernelId === snapshot.kernelId) ??
      kernels[0])
    : {
        kernelId: snapshot.kernelId,
        state: snapshot.state,
        pid: snapshot.pid,
        executionCount: snapshot.executionCount,
      };
  const activeKernelId = active.kernelId;
  const live = active.state === "running";
  // The poll loop runs while *any* kernel is running; the representative is a
  // running one whenever one exists, so `snapshot.state` tracks that.
  const sessionLive = snapshot.state === "running";
  const dotTone = live ? "bg-emerald-500" : "bg-destructive";
  const label = live ? "kernel running" : "kernel dead";

  async function onStop() {
    setStopping(true);
    try {
      // Single-kernel: stop the whole session (no id) — preserves FE-1 behavior.
      // Multi-kernel: stop just the active tab's kernel.
      await (multi && activeKernelId
        ? stop(sessionId, activeKernelId)
        : stop(sessionId));
    } finally {
      setStopping(false);
    }
  }

  async function onRestart() {
    setConfirmRestart(false);
    setRestarting(true);
    try {
      await restart(sessionId, activeKernelId ?? undefined);
    } catch (err) {
      // `notebook_restart` (backed by `KernelSupervisor::restart`, #924) can
      // still reject on a genuine backend error — e.g. a named-but-missing
      // `kernel_id`. Degrade quietly (the panel keeps showing the live kernel,
      // the next poll reconciles) rather than surfacing an unhandled rejection.
      console.debug("[notebook] restart failed:", err);
    } finally {
      setRestarting(false);
    }
  }

  return (
    <div className="flex shrink-0 flex-col border-b bg-card/40">
      {/* Toggle + controls sit as siblings in one flex row — the controls are
          real buttons, so they can't nest inside the toggle <button>. */}
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
          <span className={cn("size-1.5 shrink-0 rounded-full", dotTone)} />
          <span className="text-[11px] font-medium text-foreground">
            {label}
          </span>
          {activeKernelId && (
            <span className="font-mono text-[11px] text-muted-foreground/70">
              {activeKernelId}
            </span>
          )}
          <span className="text-[11px] text-muted-foreground/70">
            {active.executionCount} cell
            {active.executionCount === 1 ? "" : "s"} executed
          </span>
        </button>
        {sessionLive && (
          <span className="inline-flex items-center gap-1 text-[10px] text-muted-foreground/60">
            <Spinner className="size-3" />
            polling
          </span>
        )}
        <Button
          variant="outline"
          size="xs"
          className="h-6 px-2 text-[11px]"
          disabled={restarting}
          onClick={() => setConfirmRestart(true)}
          title="Restart the kernel (discards its state)"
        >
          <RotateCcw className="mr-1 size-3" />
          {restarting ? "Restarting…" : "Restart"}
        </Button>
        {live && (
          <Button
            variant="outline"
            size="xs"
            className="h-6 px-2 text-[11px]"
            disabled={stopping}
            onClick={() => void onStop()}
            title="Stop the kernel"
          >
            <Square className="mr-1 size-3" />
            {stopping ? "Stopping…" : "Stop"}
          </Button>
        )}
      </div>
      {multi && (
        <KernelTabs
          kernels={kernels}
          value={activeKernelId ?? undefined}
          onSelect={setSelectedKernelId}
        />
      )}
      {expanded && (
        <div className="space-y-1 border-t px-2.5 py-2 text-[11px] leading-relaxed text-muted-foreground/70">
          <p className="flex flex-wrap items-center gap-1.5">
            <CircleDot className="size-3" />
            <span>
              {activeKernelId ?? "(no id)"}
              {active.pid != null ? ` · pid ${active.pid}` : ""}
            </span>
          </p>
          {snapshot.raw && (
            <p className="truncate font-mono text-[10px] text-muted-foreground/60">
              {snapshot.raw}
            </p>
          )}
          {!live && (
            <p className="text-[10px]">
              Kernel is dead. Use <span className="font-medium">Restart</span>{" "}
              to spawn a fresh one, or ask the agent to call{" "}
              <code className="rounded bg-muted px-1 py-px font-mono">
                notebook_runner start
              </code>
              .
            </p>
          )}
        </div>
      )}

      <AlertDialog
        open={confirmRestart}
        onOpenChange={(next) => !next && setConfirmRestart(false)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Restart the kernel?</AlertDialogTitle>
            <AlertDialogDescription>
              This kills the current Python process and starts a fresh one. All
              in-kernel state — imported modules, variables, and the execution
              count — is discarded. Files on disk are untouched.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => void onRestart()}>
              Restart
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// Switcher for a multi-kernel session (#871 FE-2). One tab per kernel, keyed on
// kernel id, each with a live/dead dot. Selecting a tab points every header
// control (pill, Stop, Restart) at that kernel. Only rendered when a session
// holds more than one kernel, so the common single-kernel path is untouched.
function KernelTabs({
  kernels,
  value,
  onSelect,
}: {
  kernels: KernelInfo[];
  value: string | undefined;
  onSelect: (id: string) => void;
}) {
  return (
    <Tabs
      value={value}
      onValueChange={onSelect}
      className="border-b bg-card/20 px-2.5"
    >
      <TabsList className="gap-0.5 border-b-0">
        {kernels.map((k) => (
          <TabsTrigger
            key={k.kernelId}
            value={k.kernelId}
            className="gap-1.5 px-2 py-1 text-[11px]"
            title={`${k.kernelId} — ${k.state}; ${k.executionCount} cell${
              k.executionCount === 1 ? "" : "s"
            } executed`}
          >
            <span
              className={cn(
                "size-1.5 shrink-0 rounded-full",
                k.state === "running" ? "bg-emerald-500" : "bg-destructive",
              )}
            />
            <span className="font-mono">{shortKernelId(k.kernelId)}</span>
          </TabsTrigger>
        ))}
      </TabsList>
    </Tabs>
  );
}

// Compact tab label: drop the `kernel-` prefix the backend prepends, keeping the
// distinguishing suffix. Falls back to the full id if the prefix is absent.
function shortKernelId(id: string): string {
  return id.startsWith("kernel-") ? id.slice("kernel-".length) : id;
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
