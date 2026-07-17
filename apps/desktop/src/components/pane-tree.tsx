import { Fragment, useRef } from "react";
import { cn } from "@/lib/utils";
import { SessionPane } from "@/components/session-pane";
import {
  usePanesStore,
  clampRatio,
  MIN_RATIO,
  type PaneNode,
  type SplitNode,
} from "@/store/panes";

// Recursive renderer for the pane tree (#148, flattened in #985). A leaf becomes a
// SessionPane; a split lays its N children out along its axis as flex columns/rows
// interleaved with N-1 draggable dividers. Each divider resizes only its two
// neighbours — their ratios are redistributed while the rest stay fixed, so the sum
// stays 1 and every divider works at any nesting depth. Ratios are relative to the
// split's own container (not the window edge), and the flex-basis is applied
// imperatively during the drag; the store is committed + persisted once on mouseup.

const isCollapsedLeaf = (node: PaneNode) =>
  node.type === "leaf" && Boolean(node.collapsed);

const flexFor = (collapsed: boolean, ratio: number) =>
  collapsed ? "0 0 auto" : `0 0 ${ratio * 100}%`;

function PaneNodeView({
  node,
  focusedPaneId,
  canClose,
}: {
  node: PaneNode;
  focusedPaneId: string | null;
  canClose: boolean;
}) {
  const setRatios = usePanesStore((s) => s.setRatios);
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

  // Resize the boundary between child `i` and child `i+1`. Pane wrappers sit at DOM
  // indices 2*j and dividers at 2*i+1, so the neighbours are children[2*i] / [2*i+2].
  function startResize(i: number) {
    return (e: React.MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const container = containerRef.current;
      if (!container) return;
      const aEl = container.children[2 * i] as HTMLElement;
      const bEl = container.children[2 * i + 2] as HTMLElement;
      const pair = split.ratios[i] + split.ratios[i + 1];
      const leftCum = split.ratios.slice(0, i).reduce((acc, r) => acc + r, 0);
      const next = [...split.ratios];
      const onMove = (ev: MouseEvent) => {
        const rect = container.getBoundingClientRect();
        const pos = vertical
          ? (ev.clientX - rect.left) / rect.width
          : (ev.clientY - rect.top) / rect.height;
        // Clamp the near neighbour within [MIN_RATIO, pair - MIN_RATIO] so neither
        // pane in the pair collapses below the minimum.
        const a = Math.max(
          MIN_RATIO,
          Math.min(pair - MIN_RATIO, clampRatio(pos - leftCum)),
        );
        next[i] = a;
        next[i + 1] = pair - a;
        aEl.style.flex = `0 0 ${a * 100}%`;
        bEl.style.flex = `0 0 ${(pair - a) * 100}%`;
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.userSelect = "";
        document.body.style.cursor = "";
        setRatios(split.id, next);
      };
      document.body.style.userSelect = "none";
      document.body.style.cursor = vertical ? "col-resize" : "row-resize";
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    };
  }

  return (
    <div
      ref={containerRef}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 gap-1",
        vertical ? "flex-row" : "flex-col",
      )}
    >
      {split.children.map((child, i) => {
        // A divider precedes every child after the first; it's inert (a thin line)
        // when either neighbour is a collapsed leaf.
        const showDivider =
          i > 0 &&
          !isCollapsedLeaf(split.children[i - 1]) &&
          !isCollapsedLeaf(child);
        return (
          <Fragment key={child.id}>
            {i > 0 && (
              <div
                onMouseDown={showDivider ? startResize(i - 1) : undefined}
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
            )}
            <div
              className="flex min-h-0 min-w-0"
              style={{
                flex: flexFor(isCollapsedLeaf(child), split.ratios[i]),
              }}
            >
              <PaneNodeView
                node={child}
                focusedPaneId={focusedPaneId}
                canClose={canClose}
              />
            </div>
          </Fragment>
        );
      })}
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
