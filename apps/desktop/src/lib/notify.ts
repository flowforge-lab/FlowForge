// Single entry point for raising a session notification (#994). Every turn-outcome
// signal in chat.ts (done / error / approval / stopped) routes through here so the
// gating map, the sound cue, and the focus-aware title flash live in one place rather
// than being re-derived at each call site.
//
// Gating (decisions in the #994 plan):
//   master `enabled`  — gates everything; off = silent.
//   `messageComplete` — gates `done` + `stopped` (turn-ended signals).
//   `approvalRequests`— gates `approval` (approval + ask_user share this kind).
//   `error`           — master-only: a failure surfaces whenever notifications are on,
//                       never silenced by the "message complete" toggle.
//   `sound`           — adds the audio cue on top of any shown toast.
// The caller decides *whether* a session is backgrounded (chat.ts uses
// `sessionId !== activeSessionId`); notify() only decides whether the prefs allow it.

import { usePrefsStore, type NotificationPrefs } from "@/store/prefs";
import { useSessionToastStore, type ToastKind } from "@/store/session-toast";
import { playChime } from "@/lib/notification-sound";
import { flashTitle } from "@/lib/title-flash";

/** Whether `kind` is allowed given the sub-toggles (master is checked separately). */
export function allowedFor(kind: ToastKind, n: NotificationPrefs): boolean {
  switch (kind) {
    case "done":
    case "stopped":
      return n.messageComplete;
    case "approval":
      return n.approvalRequests;
    case "error":
      return true; // master-only; never gated by a sub-toggle.
  }
}

/** Raise a notification for a backgrounded session: an in-app toast, plus a sound cue
 *  and a title flash when their gates allow. No-op when prefs disallow the kind. */
export function notify(
  kind: ToastKind,
  sessionId: string,
  title: string,
): void {
  const n = usePrefsStore.getState().notifications;
  if (!n.enabled) return;
  if (!allowedFor(kind, n)) return;

  useSessionToastStore.getState().push({ kind, sessionId, title });
  if (n.sound) playChime();
  // Title flash is itself a no-op while the window is focused.
  flashTitle(kind);
}
