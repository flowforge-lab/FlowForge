// Muted banner shown in place of a raw `[stopped]` / `[stopped: …]` marker when a
// turn ended without a usable answer (#636). It replaces the literal marker text
// with a calm "Cancelled" (user pressed Stop) or the stop reason (the agent's
// cap/stall finalizer), and carries the one-click Continue affordance so the user
// can resume from the persisted conversation. Pure frontend — no IPC change.

import type { StopReason as WireStopReason } from "@/bindings/StopReason";
import { CircleOff } from "@/components/ui/icon";
import {
  ContinueAffordance,
  type StopReason,
} from "@/components/continue-affordance";

// Map the structured backend stop reason (#658) to a display label and the
// affordance's resume reason. This is the primary path for turns finalized by a
// #658-aware backend; the string-parse below is the legacy fallback for rows
// persisted before the structured column existed.
function fromStopReason(stopReason: WireStopReason): {
  label: string;
  reason: StopReason;
} {
  switch (stopReason) {
    case "cancelled":
      return { label: "Cancelled", reason: "cancelled" };
    case "toolLimit":
      return { label: "reached tool-call limit", reason: "capped" };
    case "stall":
      return {
        label: "repeated a tool call without making progress",
        reason: "capped",
      };
    case "emptyResponse":
      return {
        label: "the model returned an empty response",
        reason: "capped",
      };
    case "interrupted":
      // The turn's future was dropped mid-stream (app shutdown / hard kill) — not
      // a user Stop, so it resumes like a cap ("capped" affordance). Same resume
      // path as the legacy `[stopped: interrupted]` fallback below; the capitalized
      // label is intentional (structured rows read nicer than the verbatim marker).
      return { label: "Interrupted", reason: "capped" };
  }
}

// Resolve a stop-notice's display label + resume reason. Prefers the structured
// `stopReason` (#658); falls back to parsing the `[stopped: …]` marker text for
// legacy rows persisted before the structured column existed.
//   `[stopped]`                      -> user cancel  → "Cancelled"
//   `[stopped: reached tool-call …]` -> cap/stall    → the reason text
// The classifier upstream (isResumableStopNotice) guarantees a `[stopped` prefix;
// anything unparseable falls back to a bare "Cancelled".
export function parseStopNotice(
  content: string,
  stopReason?: WireStopReason | null,
): {
  label: string;
  reason: StopReason;
} {
  if (stopReason) return fromStopReason(stopReason);
  const match = /^\[stopped:\s*(.*?)\s*\]/s.exec(content.trim());
  if (match && match[1]) {
    return { label: match[1], reason: "capped" };
  }
  return { label: "Cancelled", reason: "cancelled" };
}

export function CancelledNotice({
  sessionId,
  content,
  stopReason,
}: {
  sessionId?: string;
  content: string;
  stopReason?: WireStopReason | null;
}) {
  const { label, reason } = parseStopNotice(content, stopReason);

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
