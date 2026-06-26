// One-click "Continue" affordance shown at the end of the transcript when the
// last turn ended at the agent's tool-call cap / a stall (#513). It saves the
// user from retyping "continue" to finish a multi-step task. Pure frontend over
// the chat store + the existing `send` path — no IPC/contract change.
//
// The store flags the session (`cappedBySession`) only after a turn ends with no
// streamed content AND the refetched final message is a reason-bearing
// `[stopped: …]` notice (see chat.ts `finishTurn`). We additionally gate on
// idle (no pending/streaming turn) so it never overlaps the thinking indicator
// or an active stream.

import { CornerDownLeft } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/store/chat";

export function ContinueAffordance({ sessionId }: { sessionId?: string }) {
  const capped = useChatStore((s) =>
    sessionId ? Boolean(s.cappedBySession[sessionId]) : false,
  );
  const busy = useChatStore((s) =>
    sessionId
      ? Boolean(s.turnStartBySession[sessionId]) ||
        Boolean(s.streamingBySession[sessionId])
      : false,
  );
  const send = useChatStore((s) => s.send);

  // Only when the turn is genuinely capped and the session is idle. Clicking
  // sends "continue"; `send` clears the flag and flips the session busy, so the
  // button disappears immediately — no extra double-send guard needed.
  if (!sessionId || !capped || busy) return null;

  return (
    <div className="flex justify-start">
      <Button
        type="button"
        variant="outline"
        size="xs"
        className="text-muted-foreground hover:text-foreground"
        onClick={() => void send("continue", sessionId)}
        title="Resume the task — the previous turn stopped at the tool-call limit"
      >
        <CornerDownLeft className="size-3" />
        Continue
      </Button>
    </div>
  );
}
