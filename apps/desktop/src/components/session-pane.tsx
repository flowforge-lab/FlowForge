import { useRef, useState } from "react";
import {
  Columns2,
  EyeOff,
  Folder,
  Rows2,
  Search,
  SquareTerminal,
  Upload,
  X,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ChatView } from "@/components/chat-view";
import { ContextGauge } from "@/components/context-gauge";
import { FilePanel } from "@/components/file-panel";
import { FindBar } from "@/components/find-bar";
import { TerminalDrawer } from "@/components/terminal";
import { GoalStatusPanel } from "@/components/goal-status-panel";
import { NotebookStatusPanel } from "@/components/notebook-status-panel";
import { ObserverPanel } from "@/components/observer-panel";
import { ProcessStatusPanel } from "@/components/process-status-panel";
import { InputBar } from "@/components/input-bar";
import { PhenoSelector } from "@/components/pheno-selector";
import { useAttachGate } from "@/lib/attach-gate";
import { stageFiles } from "@/lib/stage-files";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { clampPanelWidth, useFilePanelStore } from "@/store/file-panel";
import { useFindStore } from "@/store/find";
import { usePanesStore, MAX_PANES } from "@/store/panes";
import { clampDrawerHeight, useTerminalStore } from "@/store/terminal";

// A single tiling pane (#148): one independent session rendered as a full chat
// column with its own header controls. The header carries open-new-session-split /
// close actions (#1069 repurposed the split buttons from fork to plain new-session,
// since the sidebar is now the single fork entry point); the whole pane is
// click-to-focus and shows a focus ring when it's the active pane. Splitting/closing
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
  const splitNew = usePanesStore((s) => s.splitNew);
  const closePane = usePanesStore((s) => s.closePane);
  const atCap = usePanesStore((s) => s.leafCount() >= MAX_PANES);

  const toggleFind = useFindStore((s) => s.toggleFind);
  const findOpen = useFindStore((s) => s.open && s.sessionId === sessionId);
  // Scopes the find bar's occurrence search to this pane's transcript so
  // highlights never leak across split panes (#679).
  const contentRef = useRef<HTMLDivElement>(null);

  // Per-pane file browser (#944): open state + divider width live in the
  // file-panel store, keyed by this pane's session so panes stay independent.
  // The store hydrates asynchronously (#1134), so `openSessions` is empty for a
  // tick after mount — hold the panel until it lands rather than painting the
  // chat full-width and snapping the panel in, and so `FilePanel`'s mount-time
  // `syncSession` sees the restored expanded dirs instead of none.
  const filesOpen = useFilePanelStore(
    (s) => s.hasHydrated && s.openSessions.has(sessionId),
  );
  const toggleFiles = useFilePanelStore((s) => s.toggleFiles);
  const panelWidth = useFilePanelStore((s) => s.panelWidth);
  const setPanelWidth = useFilePanelStore((s) => s.setPanelWidth);

  // Per-pane terminal drawer (#1284): open state + height live in the terminal
  // store, keyed by this pane's session so panes get independent shells. Held
  // until the store hydrates for the same reason as the file panel above — a
  // restored drawer must not snap in a tick after the transcript has painted.
  const terminalOpen = useTerminalStore(
    (s) => s.hasHydrated && s.openSessions.has(sessionId),
  );
  const toggleDrawer = useTerminalStore((s) => s.toggleDrawer);
  const drawerHeight = useTerminalStore((s) => s.drawerHeight);
  const setDrawerHeight = useTerminalStore((s) => s.setDrawerHeight);

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

  // Clicking a background pane used to move the focus ring but drop keyboard focus to
  // <body>, so the next keystroke went nowhere and the user had to click the composer
  // as a second click (#1122). Hand the caret to this pane's composer once the click
  // completes, via the existing focus-nonce bridge the input bar already listens on.
  //
  // On `click` (mouseup), not the mousedown that moves the ring: only by then is a
  // text selection materialized, so dragging to select transcript text can be excluded
  // — input-bar.tsx keeps the rule that merely focusing a pane must not yank the caret.
  const wasFocusedOnDownRef = useRef(focused);
  const requestFocus = useComposerStore((s) => s.requestFocus);
  const handleClick = (e: React.MouseEvent) => {
    // Already the focused pane: the user is interacting inside it, not switching.
    if (wasFocusedOnDownRef.current) return;
    // Interactive targets keep their native behavior — clicking a background pane's
    // button, link, or its own composer must not be overridden.
    const target = e.target as HTMLElement | null;
    if (
      target?.closest(
        "input, textarea, button, a, [role='button'], [contenteditable='true']",
      )
    ) {
      return;
    }
    // The click ended a drag-selection of transcript text; leave that selection alone.
    const selection = target?.ownerDocument.defaultView?.getSelection();
    if (selection && !selection.isCollapsed) return;
    requestFocus(sessionId);
  };

  // Drag-to-resize the chat|files divider. The file panel is flush to the pane's
  // right edge, so its width is the distance from the cursor to that edge.
  // Applied imperatively during the drag; committed once on mouseup (mirrors
  // split-panel.tsx / pane-tree.tsx).
  function startFilesResize(e: React.MouseEvent) {
    e.preventDefault();
    const panel = e.currentTarget.nextElementSibling as HTMLElement | null;
    const paneRight =
      e.currentTarget.parentElement?.getBoundingClientRect().right ?? 0;
    let latest = panelWidth;
    const onMove = (ev: MouseEvent) => {
      latest = clampPanelWidth(paneRight - ev.clientX);
      if (panel) panel.style.width = `${latest}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setPanelWidth(latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  // Drag-to-resize the transcript|terminal divider. Same shape as
  // `startFilesResize`, rotated: the drawer is flush to the pane's bottom edge,
  // so its height is the distance from that edge up to the cursor.
  function startTerminalResize(e: React.MouseEvent) {
    e.preventDefault();
    const drawer = e.currentTarget.nextElementSibling as HTMLElement | null;
    const paneBottom =
      e.currentTarget.parentElement?.getBoundingClientRect().bottom ?? 0;
    let latest = drawerHeight;
    const onMove = (ev: MouseEvent) => {
      latest = clampDrawerHeight(paneBottom - ev.clientY);
      if (drawer) drawer.style.height = `${latest}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setDrawerHeight(latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "row-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  return (
    <div
      onMouseDownCapture={() => {
        wasFocusedOnDownRef.current = focused;
        if (!focused) focusPane(paneId);
      }}
      onClick={handleClick}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-lg border bg-background transition-colors",
        focused ? "ring-2 ring-ring" : "border-border",
      )}
    >
      <div className="flex h-8 shrink-0 items-center justify-between gap-2 border-b bg-card/50 px-2">
        <div className="flex min-w-0 flex-1 items-center gap-1.5">
          {/* Phenotype selector (#245 2b, per-session #935) — picks the working
              set this pane's session runs as, bound per session. */}
          <PhenoSelector sessionId={sessionId} />
          <span className="min-w-0 truncate text-xs font-medium text-muted-foreground">
            {title}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {/* Estimated context usage for this session (#282). Self-hides until
              the first turn completes with an estimate. */}
          <ContextGauge sessionId={sessionId} />
          {/* Files panel (#872, per-pane #944): a visible entry point besides
              the palette / ⌘⇧E. Toggles the workspace browser for THIS pane's
              session, scoped so other panes are unaffected. */}
          <Button
            variant="ghost"
            size="icon-xs"
            aria-pressed={filesOpen}
            title="Toggle Files (⌘⇧E)"
            onClick={() => toggleFiles(sessionId)}
          >
            <Folder className="size-3.5" />
          </Button>
          {/* Terminal drawer (#1284): an interactive shell rooted at THIS pane's
              session working directory, so the shell opens where the agent works. */}
          <Button
            variant="ghost"
            size="icon-xs"
            aria-pressed={terminalOpen}
            title="Toggle Terminal (⌘J)"
            onClick={() => toggleDrawer(sessionId)}
          >
            <SquareTerminal className="size-3.5" />
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
          {/* Open a new (empty) session in a split (#1069: split is a layout
              gesture, not a fork — fork lives only in the sidebar now). */}
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Open new session right"
            onClick={() => void splitNew(paneId, "vertical")}
          >
            <Columns2 className="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Open new session down"
            onClick={() => void splitNew(paneId, "horizontal")}
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
      {/* Live background-process output (#873 FE / #987): self-hides unless this
          session has a process started via `process_manager`. Sits above the
          kernel/goal panels so long-running dev servers stay visible. */}
      <ProcessStatusPanel sessionId={sessionId} />

      {/* Active observers (#1038 / epic #954 M2): self-hides unless this session
          has background observers the agent attached; lists them with a stop
          [×] and live-updates via `observer:changed`. */}
      <ObserverPanel sessionId={sessionId} />

      <NotebookStatusPanel sessionId={sessionId} />

      {/* Goal status panel (#717): self-hides unless this session has a goal. */}
      <GoalStatusPanel sessionId={sessionId} />

      {/* Body: chat column, plus the per-pane file browser as an optional right
          section (#944) with a drag handle between them. */}
      <div className="flex min-h-0 min-w-0 flex-1">
        <div
          ref={contentRef}
          data-testid="pane-dropzone"
          className="relative flex min-h-0 min-w-0 flex-1 flex-col"
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
        >
          {findOpen && <FindBar sessionId={sessionId} rootRef={contentRef} />}
          <ChatView sessionId={sessionId} />
          <InputBar sessionId={sessionId} focused={focused} />
          {dragOver && <DropOverlay gated={gate.attachGated} />}
        </div>

        {filesOpen && (
          <>
            {/* Resize handle between the chat column and the file panel. */}
            <div
              onMouseDown={startFilesResize}
              title="Drag to resize"
              className="w-1 shrink-0 cursor-col-resize rounded-full transition-colors hover:bg-primary/30"
            />
            <aside
              style={{ width: panelWidth }}
              className="flex min-h-0 shrink-0 flex-col border-l bg-card"
            >
              <FilePanel sessionId={sessionId} />
            </aside>
          </>
        )}
      </div>

      {/* Terminal drawer (#1284): the full width of the pane, *below* the
          chat|files row, so a shell and the file browser can be open at once
          without fighting for the same space. */}
      {terminalOpen && (
        <>
          {/* Resize handle between the pane body and the drawer. */}
          <div
            onMouseDown={startTerminalResize}
            title="Drag to resize"
            className="h-1 shrink-0 cursor-row-resize rounded-full transition-colors hover:bg-primary/30"
          />
          <section
            style={{ height: drawerHeight }}
            className="min-h-0 shrink-0"
            aria-label="Terminal"
          >
            <TerminalDrawer sessionId={sessionId} />
          </section>
        </>
      )}
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
