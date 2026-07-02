// One-click "Continue" affordance shown when the last turn ended without a usable
// answer the user can resume: the agent hit its tool-call cap / stalled (#513), or
// the user pressed Stop (#636). It saves retyping "continue" to finish the task.
// Pure frontend over the chat store + the existing `send` path — no IPC/contract
// change. Rendered inside the Cancelled/Stopped banner (see cancelled-notice.tsx).
//
// The store flags the session (`resumableBySession`) only after a turn ends with no
// streamed content AND the refetched final message is a resumable stop notice (see
// chat.ts `finishTurn`). We additionally gate on idle (no pending/streaming turn) so
// it never overlaps the thinking indicator or an active stream.

import { CornerDownLeft } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/store/chat";

// Why the last turn stopped — drives the button tooltip copy. "cancelled" = the
// user pressed Stop (bare `[stopped]`); "capped" = the agent's cap/stall finalizer.
export type StopReason = "cancelled" | "capped";

const TOOLTIP: Record<StopReason, string> = {
  cancelled: "Resume — you stopped this turn",
  capped: "Resume the task — the previous turn stopped at the tool-call limit",
};

export function ContinueAffordance({
  sessionId,
  reason = "capped",
}: {
  sessionId?: string;
  reason?: StopReason;
}) {
  const resumable = useChatStore((s) =>
    sessionId ? Boolean(s.resumableBySession[sessionId]) : false,
  );
  const busy = useChatStore((s) =>
    sessionId
      ? Boolean(s.turnStartBySession[sessionId]) ||
        Boolean(s.streamingBySession[sessionId])
      : false,
  );
  const send = useChatStore((s) => s.send);

  // Only when the turn is genuinely resumable and the session is idle. Clicking
  // sends "continue"; `send` clears the flag and flips the session busy, so the
  // button disappears immediately — no extra double-send guard needed.
  if (!sessionId || !resumable || busy) return null;

  return (
    <div className="flex justify-start">
      <Button
        type="button"
        variant="outline"
        size="xs"
        className="text-muted-foreground hover:text-foreground"
        onClick={() => void send("continue", sessionId)}
        title={TOOLTIP[reason]}
      >
        <CornerDownLeft className="size-3" />
        Continue
      </Button>
    </div>
  );
}
