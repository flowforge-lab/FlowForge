import { useRef } from "react";
import { cn } from "@/lib/utils";
import { SessionPane } from "@/components/session-pane";
import {
  usePanesStore,
  clampRatio,
  type PaneNode,
  type SplitNode,
} from "@/store/panes";

// Recursive renderer for the pane tree (#148). A leaf becomes a SessionPane; a
// split lays its two children out along its axis with a draggable divider. The
// divider commits a 0..1 ratio relative to the split's container (not the window
// edge like SplitPanel) so nested splits resize correctly. During the drag the
// flex-basis is applied imperatively — the store is committed + persisted once on
// mouseup (mirrors split-panel.tsx's startResize).

function PaneNodeView({
  node,
  focusedPaneId,
  canClose,
}: {
  node: PaneNode;
  focusedPaneId: string | null;
  canClose: boolean;
}) {
  const setRatio = usePanesStore((s) => s.setRatio);
  const containerRef = useRef<HTMLDivElement>(null);

  if (node.type === "leaf") {
    return (
      <SessionPane
        paneId={node.id}
        sessionId={node.sessionId}
        focused={node.id === focusedPaneId}
        canClose={canClose}
      />
    );
  }

  const split: SplitNode = node;
  const vertical = split.dir === "vertical"; // side-by-side
  const aCollapsed = split.a.type === "leaf" && split.a.collapsed;
  const bCollapsed = split.b.type === "leaf" && split.b.collapsed;
  const showDivider = !aCollapsed && !bCollapsed;

  function startResize(e: React.MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const container = containerRef.current;
    if (!container) return;
    const aEl = container.children[0] as HTMLElement;
    const bEl = container.children[2] as HTMLElement; // [0]=a, [1]=divider, [2]=b
    let latest = split.ratio;
    const onMove = (ev: MouseEvent) => {
      const rect = container.getBoundingClientRect();
      const pos = vertical
        ? (ev.clientX - rect.left) / rect.width
        : (ev.clientY - rect.top) / rect.height;
      latest = clampRatio(pos);
      const aPct = `${latest * 100}%`;
      const bPct = `${(1 - latest) * 100}%`;
      aEl.style.flex = `0 0 ${aPct}`;
      bEl.style.flex = `0 0 ${bPct}`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setRatio(split.id, latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = vertical ? "col-resize" : "row-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  const flexFor = (collapsed: boolean, ratio: number) =>
    collapsed ? "0 0 auto" : `0 0 ${ratio * 100}%`;

  return (
    <div
      ref={containerRef}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 gap-1",
        vertical ? "flex-row" : "flex-col",
      )}
    >
      <div
        className="flex min-h-0 min-w-0"
        style={{ flex: flexFor(Boolean(aCollapsed), split.ratio) }}
      >
        <PaneNodeView
          node={split.a}
          focusedPaneId={focusedPaneId}
          canClose={canClose}
        />
      </div>

      <div
        onMouseDown={showDivider ? startResize : undefined}
        title={showDivider ? "Drag to resize" : undefined}
        className={cn(
          "shrink-0 rounded-full transition-colors",
          showDivider
            ? vertical
              ? "w-1 cursor-col-resize hover:bg-primary/30"
              : "h-1 cursor-row-resize hover:bg-primary/30"
            : vertical
              ? "w-px bg-border"
              : "h-px bg-border",
        )}
      />

      <div
        className="flex min-h-0 min-w-0"
        style={{ flex: flexFor(Boolean(bCollapsed), 1 - split.ratio) }}
      >
        <PaneNodeView
          node={split.b}
          focusedPaneId={focusedPaneId}
          canClose={canClose}
        />
      </div>
    </div>
  );
}

export function PaneTree() {
  const root = usePanesStore((s) => s.root);
  const focusedPaneId = usePanesStore((s) => s.focusedPaneId);
  const leafCount = usePanesStore((s) => s.leafCount());

  if (!root) return null;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col p-2">
      <PaneNodeView
        node={root}
        focusedPaneId={focusedPaneId}
        canClose={leafCount > 1}
      />
    </div>
  );
}
