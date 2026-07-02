// Muted banner shown in place of a raw `[stopped]` / `[stopped: …]` marker when a
// turn ended without a usable answer (#636). It replaces the literal marker text
// with a calm "Cancelled" (user pressed Stop) or the stop reason (the agent's
// cap/stall finalizer), and carries the one-click Continue affordance so the user
// can resume from the persisted conversation. Pure frontend — no IPC change.

import { CircleOff } from "@/components/ui/icon";
import {
  ContinueAffordance,
  type StopReason,
} from "@/components/continue-affordance";

// Parse a stop-notice marker into a display label and the resume reason.
//   `[stopped]`                      -> user cancel  → "Cancelled"
//   `[stopped: reached tool-call …]` -> cap/stall    → the reason text
// The classifier upstream (isResumableStopNotice) guarantees a `[stopped` prefix;
// anything unparseable falls back to a bare "Stopped".
export function parseStopNotice(content: string): {
  label: string;
  reason: StopReason;
} {
  const match = /^\[stopped:\s*(.*?)\s*\]/s.exec(content.trim());
  if (match && match[1]) {
    return { label: match[1], reason: "capped" };
  }
  return { label: "Cancelled", reason: "cancelled" };
}

export function CancelledNotice({
  sessionId,
  content,
}: {
  sessionId?: string;
  content: string;
}) {
  const { label, reason } = parseStopNotice(content);

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-1.5 px-0.5 py-1 text-sm leading-relaxed text-muted-foreground">
        <CircleOff className="size-3.5 shrink-0" />
        <span>{label}</span>
      </div>
      <ContinueAffordance sessionId={sessionId} reason={reason} />
    </div>
  );
}
