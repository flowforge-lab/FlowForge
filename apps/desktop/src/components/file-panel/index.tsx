import { useEffect } from "react";
import { X } from "@/components/ui/icon";
import { clampTreeWidth, useFilePanelStore } from "@/store/file-panel";
import { FileTree } from "./file-tree";
import { FileViewer } from "./file-viewer";

// Files panel shell (#872, per-pane in #944): a workspace file tree on the left,
// the file viewer on the right, with a drag handle between them. Rendered inside
// each session pane (see session-pane.tsx) and scoped to that pane's session, so
// two panes browse independently. Points the store at `sessionId` on mount /
// change, which loads the root listing and re-hydrates persisted expansion +
// selection.

export function FilePanel({ sessionId }: { sessionId: string }) {
  const syncSession = useFilePanelStore((s) => s.syncSession);
  const closeFiles = useFilePanelStore((s) => s.closeFiles);
  const treeWidth = useFilePanelStore((s) => s.treeWidth);
  const setTreeWidth = useFilePanelStore((s) => s.setTreeWidth);

  useEffect(() => {
    void syncSession(sessionId);
  }, [sessionId, syncSession]);

  // Drag-to-resize the tree column: width is the distance from the file panel's
  // left edge to the cursor. Applied imperatively during the drag (no React
  // render / localStorage write per mousemove); committed once on mouseup
  // (mirrors split-panel.tsx / pane-tree.tsx).
  function startResize(e: React.MouseEvent) {
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const treeEl = handle.previousElementSibling as HTMLElement | null;
    const panelLeft = handle.parentElement?.getBoundingClientRect().left ?? 0;
    let latest = treeWidth;
    const onMove = (ev: MouseEvent) => {
      latest = clampTreeWidth(ev.clientX - panelLeft);
      if (treeEl) treeEl.style.width = `${latest}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setTreeWidth(latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  return (
    <div className="flex h-full w-full min-w-0 flex-col">
      <div className="flex h-9 shrink-0 items-center justify-between gap-2 border-b px-3">
        <span className="text-[13px] font-medium text-foreground">Files</span>
        <button
          type="button"
          title="Close Files (⌘⇧E)"
          onClick={() => closeFiles(sessionId)}
          className="flex size-6 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
        >
          <X className="size-3.5" />
        </button>
      </div>
      <div className="flex min-h-0 w-full min-w-0 flex-1">
        <div
          style={{ width: treeWidth }}
          className="shrink-0 overflow-auto border-r bg-card/50"
        >
          <FileTree sessionId={sessionId} />
        </div>
        {/* Resize handle between the tree and the viewer (stays adjacent to the
            tree during the drag since it's the next in-flow sibling). */}
        <div
          onMouseDown={startResize}
          title="Drag to resize"
          className="w-1 shrink-0 cursor-col-resize rounded-full transition-colors hover:bg-primary/30"
        />
        <div className="min-w-0 flex-1">
          <FileViewer sessionId={sessionId} />
        </div>
      </div>
    </div>
  );
}
