import { SplitSquareHorizontal, SplitSquareVertical, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ChatView } from "@/components/chat-view";
import { InputBar } from "@/components/input-bar";
import { useChatStore } from "@/store/chat";
import { usePanesStore, MAX_PANES } from "@/store/panes";

// A single tiling pane (#148): one independent session rendered as a full chat
// column with its own header controls. The header carries split / close actions;
// the whole pane is click-to-focus and shows a focus ring when it's the active
// pane. Splitting/closing routes through the panes store; the session content is
// just <ChatView sessionId> + <InputBar sessionId>, both already session-scoped.
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

  const title = useChatStore((s) => {
    const session = s.sessions.find((x) => x.id === sessionId);
    return session?.title || s.sessionTitles[sessionId] || "New session";
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
        <span className="min-w-0 truncate text-xs font-medium text-muted-foreground">
          {title}
        </span>
        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Split right"
            onClick={() => void splitNew(paneId, "vertical")}
          >
            <SplitSquareHorizontal className="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={atCap}
            title="Split down"
            onClick={() => void splitNew(paneId, "horizontal")}
          >
            <SplitSquareVertical className="size-3.5" />
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
