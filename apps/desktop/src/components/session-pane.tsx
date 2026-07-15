import { useRef, useState } from "react";
import {
  Columns2,
  EyeOff,
  Folder,
  Rows2,
  Search,
  Upload,
  X,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ChatView } from "@/components/chat-view";
import { ContextGauge } from "@/components/context-gauge";
import { FindBar } from "@/components/find-bar";
import { GoalStatusPanel } from "@/components/goal-status-panel";
import { NotebookStatusPanel } from "@/components/notebook-status-panel";
import { InputBar } from "@/components/input-bar";
import { PhenoSelector } from "@/components/pheno-selector";
import { useAttachGate } from "@/lib/attach-gate";
import { stageFiles } from "@/lib/stage-files";
import { useChatStore } from "@/store/chat";
import { useFilePanelStore } from "@/store/file-panel";
import { useFindStore } from "@/store/find";
import { usePanesStore, MAX_PANES } from "@/store/panes";

// A single tiling pane (#148): one independent session rendered as a full chat
// column with its own header controls. The header carries fork (duplicate) / close
// actions (#204 dropped the redundant Split buttons); the whole pane is
// click-to-focus and shows a focus ring when it's the active pane. Forking/closing
// routes through the panes store; the session content is just <ChatView sessionId>
// + <InputBar sessionId>, both already session-scoped.
export function SessionPane({
  paneId,
  sessionId,
  focused,
  canClose,
}: {
  paneId: string;
  sessionId: string;
  focused: boolean;
  canClose: boolean;
}) {
  const focusPane = usePanesStore((s) => s.focusPane);
  const splitFork = usePanesStore((s) => s.splitFork);
  const closePane = usePanesStore((s) => s.closePane);
  const atCap = usePanesStore((s) => s.leafCount() >= MAX_PANES);

  const toggleFind = useFindStore((s) => s.toggleFind);
  const findOpen = useFindStore((s) => s.open && s.sessionId === sessionId);
  // Scopes the find bar's occurrence search to this pane's transcript so
  // highlights never leak across split panes (#679).
  const contentRef = useRef<HTMLDivElement>(null);

  const title = useChatStore((s) => {
    const session = s.sessions.find((x) => x.id === sessionId);
    return session?.title || "New session";
  });

  // Region-wide attachment drag-and-drop (#723). The whole pane content is the
  // drop target — dropping anywhere here stages to THIS pane's composer, so split
  // view resolves ownership by which pane the cursor is over (no global target).
  // The gate is shared with the input bar; when it forbids all attachments the
  // overlay shows a disabled state and nothing stages.
  const gate = useAttachGate(sessionId);
  const [dragOver, setDragOver] = useState(false);

  // Only react to file drags, never text/element drags within the transcript.
  const isFileDrag = (e: React.DragEvent) =>
    Array.from(e.dataTransfer?.types ?? []).includes("Files");

  const handleDragOver = (e: React.DragEvent) => {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    setDragOver(true);
  };

  const handleDragLeave = (e: React.DragEvent) => {
    // Only clear when leaving the pane entirely, not when crossing a child.
    if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragOver(false);
  };

  const handleDrop = (e: React.DragEvent) => {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    setDragOver(false);
    if (!focused) focusPane(paneId);
    // stageFiles gates per file, so a fully-gated model simply stages nothing and
    // surfaces the reason — the overlay's disabled state already signalled it.
    stageFiles(sessionId, Array.from(e.dataTransfer.files), gate);
  };

  return (
    <div
      onMouseDownCapture={() => {
        if (!focused) focusPane(paneId);
      }}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-lg border bg-background transition-colors",
        focused ? "ring-2 ring-ring" : "border-border",
      )}
    >
      <div className="flex h-8 shrink-0 items-center justify-between gap-2 border-b bg-card/50 px-2">
        <div className="flex min-w-0 flex-1 items-center gap-1.5">
          {/* Phenotype selector (#245 2b) — picks the working set this session
              runs as. v1 switches the global active phenotype; see PhenoSelector. */}
          <PhenoSelector />
          <span className="min-w-0 truncate text-xs font-medium text-muted-foreground">
            {title}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {/* Estimated context usage for this session (#282). Self-hides until
              the first turn completes with an estimate. */}
          <ContextGauge sessionId={sessionId} />
          {/* Files panel (#872): a visible entry point besides the palette /
              ⌘⇧E. Opens the workspace browser for the active session. */}
          <Button
            variant="ghost"
            size="icon-xs"
            title="Open Files (⌘⇧E)"
            onClick={() => useFilePanelStore.getState().openFiles()}
          >
            <Folder className="size-3.5" />
          </Button>
          {/* Find in thread (#679): Cmd/Ctrl+F also toggles this. */}
          <Button
            variant="ghost"
            size="icon-xs"
            aria-pressed={findOpen}
            title="Find in thread (⌘F)"
            onClick={() => toggleFind(sessionId)}
          >
            <Search className="size-3.5" />
          </Button>
          {/* Fork: duplicate this pane's session into the new pane (#149). */}
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Duplicate right (fork session)"
            onClick={() => void splitFork(paneId, "vertical")}
          >
            <Columns2 className="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Duplicate down (fork session)"
            onClick={() => void splitFork(paneId, "horizontal")}
          >
            <Rows2 className="size-3.5" />
          </Button>

          <Button
            variant="ghost"
            size="icon-xs"
            disabled={!canClose}
            title="Close pane"
            onClick={() => closePane(paneId)}
          >
            <X className="size-3.5" />
          </Button>
        </div>
      </div>

      {/* Notebook kernel status panel (#871 FE-1): self-hides when the session
          has no kernel snapshot yet, renders a quiet line for `null`, and a
          live pill + Stop button while a kernel is `running`. Polls
          `notebook_status` for the duration of the `running` state (cadence
          tunable, see experimental store). Sits above the goal panel so
          kernel + goal state both stay visible. */}
      <NotebookStatusPanel sessionId={sessionId} />

      {/* Goal status panel (#717): self-hides unless this session has a goal. */}
      <GoalStatusPanel sessionId={sessionId} />

      <div
        ref={contentRef}
        data-testid="pane-dropzone"
        className="relative flex min-h-0 flex-1 flex-col"
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
      >
        {findOpen && <FindBar sessionId={sessionId} rootRef={contentRef} />}
        <ChatView sessionId={sessionId} />
        <InputBar sessionId={sessionId} focused={focused} />
        {dragOver && <DropOverlay gated={gate.attachGated} />}
      </div>
    </div>
  );
}

// The drag affordance shown while a file is dragged over the pane (#723).
// `pointer-events-none` so drag events keep hitting the container underneath
// (the drop target), not the overlay. Disabled variant when the model can't
// accept any attachment kind.
function DropOverlay({ gated }: { gated: boolean }) {
  return (
    <div
      data-testid="drop-overlay"
      className={cn(
        "pointer-events-none absolute inset-0 z-30 flex items-center justify-center rounded-lg border-2 border-dashed backdrop-blur-sm",
        gated
          ? "border-muted-foreground/40 bg-muted/40"
          : "border-primary bg-primary/10",
      )}
    >
      <div className="flex flex-col items-center gap-2 text-center">
        {gated ? (
          <>
            <EyeOff className="size-6 text-muted-foreground" />
            <p className="max-w-56 text-sm font-medium text-muted-foreground">
              This model can&apos;t accept attachments
            </p>
          </>
        ) : (
          <>
            <Upload className="size-6 text-primary" />
            <p className="text-sm font-medium text-foreground">
              Drop files to attach
            </p>
          </>
        )}
      </div>
    </div>
  );
}
