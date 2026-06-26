import { Columns2, Rows2, X } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ChatView } from "@/components/chat-view";
import { ContextGauge } from "@/components/context-gauge";
import { InputBar } from "@/components/input-bar";
import { PhenoSelector } from "@/components/pheno-selector";
import { useChatStore } from "@/store/chat";
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

  const title = useChatStore((s) => {
    const session = s.sessions.find((x) => x.id === sessionId);
    return session?.title || "New session";
  });

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

      <div className="flex min-h-0 flex-1 flex-col">
        <ChatView sessionId={sessionId} />
        <InputBar sessionId={sessionId} focused={focused} />
      </div>
    </div>
  );
}
