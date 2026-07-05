// Completion toasts for background sessions (#703). When a turn finishes on a
// session the user isn't viewing, chat.finishTurn enqueues a toast here; each
// card announces the session and offers a one-click "View" to jump to it.
// Mirrors <PhenoMcpToast>: the store owns the queue, this renders it. Auto-
// dismiss (10s) is owned per-card here, matching the ui/toast.tsx contract.

import { useEffect } from "react";
import { Check, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Toast, ToastViewport } from "@/components/ui/toast";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import {
  useSessionDoneToastStore,
  type DoneToast,
} from "@/store/session-done-toast";

const AUTO_DISMISS_MS = 10_000;

export function SessionDoneToast() {
  const toasts = useSessionDoneToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;

  return (
    // Top-right, per #703 — distinct from the sticky MCP notice's bottom-right
    // slot, so the two never overlap. `bottom-auto` clears the viewport default.
    <ToastViewport className="top-4 bottom-auto">
      <div className="flex flex-col gap-2">
        {toasts.map((toast) => (
          <SessionDoneToastCard key={toast.id} toast={toast} />
        ))}
      </div>
    </ToastViewport>
  );
}

function SessionDoneToastCard({ toast }: { toast: DoneToast }) {
  const dismiss = useSessionDoneToastStore((s) => s.dismiss);
  const selectSession = useChatStore((s) => s.selectSession);
  const loadSession = useChatStore((s) => s.loadSession);

  // Self-dismiss after the window unless the user acts first. Re-armed if the
  // card's id changes (it won't — cards are keyed by id — but keeps the effect
  // honest).
  useEffect(() => {
    const t = setTimeout(() => dismiss(toast.id), AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [toast.id, dismiss]);

  const view = () => {
    // Pane-aware, mirroring the sidebar row's open(): land in the focused pane
    // when panes are active, else switch the global active session.
    const focused = usePanesStore.getState().focusedPaneId;
    if (focused) {
      usePanesStore.getState().setPaneSession(focused, toast.sessionId);
      void loadSession(toast.sessionId);
    } else {
      void selectSession(toast.sessionId);
    }
    dismiss(toast.id);
  };

  return (
    <Toast>
      <div className="flex items-start gap-2.5">
        <Check className="mt-0.5 size-4 shrink-0 text-emerald-500" />
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-medium">{toast.title}</p>
          <p className="mt-0.5 text-[12px] text-muted-foreground">Finished</p>
          <div className="mt-2">
            <Button
              size="sm"
              className="h-7 bg-emerald-600 text-xs text-white hover:bg-emerald-600/90"
              onClick={view}
            >
              View
            </Button>
          </div>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="-mr-1 -mt-1 size-6 shrink-0 text-muted-foreground hover:text-foreground"
          onClick={() => dismiss(toast.id)}
          title="Dismiss"
          aria-label="Dismiss"
        >
          <X className="size-3.5" />
        </Button>
      </div>
    </Toast>
  );
}
