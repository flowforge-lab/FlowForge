// Session-notification toasts for background sessions (#703, expanded in #994).
// When something happens on a session the user isn't viewing — a turn finished,
// errored, stopped without an answer, or is blocked awaiting approval — chat.ts (via
// lib/notify.ts) enqueues a toast here; each card announces the session, colour-codes
// the severity, and offers a one-click action to jump to it. Mirrors <PhenoMcpToast>:
// the store owns the queue, this renders it.
//
// Auto-dismiss is per-kind: `done`/`stopped` fade after 10s; `approval`/`error` are
// sticky (they need the user's attention) and clear only on the action, an explicit
// dismiss, or when the session becomes active (session-sidebar's dismissBySession).

import { useEffect } from "react";
import {
  Check,
  X,
  AlertTriangle,
  ShieldAlert,
  Pause,
  CornerDownLeft,
  type LucideIcon,
} from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Toast, ToastViewport } from "@/components/ui/toast";
import { cn } from "@/lib/utils";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import {
  useSessionToastStore,
  type SessionToast,
  type ToastKind,
} from "@/store/session-toast";

const AUTO_DISMISS_MS = 10_000;

interface KindConfig {
  icon: LucideIcon;
  /** Icon tint. */
  iconClass: string;
  /** Severity accent border on the card (visible in light + dark). */
  borderClass: string;
  /** Solid action-button background. */
  actionClass: string;
  /** Sub-line under the session title. */
  label: string;
  /** Action button copy. */
  action: string;
  /** `done`/`stopped` self-dismiss; `approval`/`error` stay until acted on. */
  sticky: boolean;
}

const KIND: Record<ToastKind, KindConfig> = {
  done: {
    icon: Check,
    iconClass: "text-emerald-500",
    borderClass: "border-l-emerald-500/50",
    actionClass: "bg-emerald-600 text-white hover:bg-emerald-600/90",
    label: "Finished",
    action: "View",
    sticky: false,
  },
  approval: {
    icon: ShieldAlert,
    iconClass: "text-amber-500",
    borderClass: "border-l-amber-500/60",
    actionClass: "bg-amber-500 text-white hover:bg-amber-500/90",
    label: "Needs your approval",
    action: "Review",
    sticky: true,
  },
  error: {
    icon: AlertTriangle,
    iconClass: "text-red-500",
    borderClass: "border-l-red-500/60",
    actionClass: "bg-red-600 text-white hover:bg-red-600/90",
    label: "Failed",
    action: "View",
    sticky: true,
  },
  stopped: {
    icon: Pause,
    iconClass: "text-red-500",
    borderClass: "border-l-red-500/50",
    actionClass: "bg-red-600 text-white hover:bg-red-600/90",
    label: "Stopped — continue where it left off",
    action: "Continue",
    sticky: false,
  },
};

export function SessionToasts() {
  const toasts = useSessionToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;

  return (
    // Top-right, per #703 — distinct from the sticky MCP notice's bottom-right
    // slot, so the two never overlap. `bottom-auto` clears the viewport default.
    <ToastViewport className="top-4 bottom-auto">
      <div className="flex flex-col gap-2">
        {toasts.map((toast) => (
          <SessionToastCard key={toast.id} toast={toast} />
        ))}
      </div>
    </ToastViewport>
  );
}

function SessionToastCard({ toast }: { toast: SessionToast }) {
  const dismiss = useSessionToastStore((s) => s.dismiss);
  const selectSession = useChatStore((s) => s.selectSession);
  const loadSession = useChatStore((s) => s.loadSession);
  const cfg = KIND[toast.kind];
  const Icon = cfg.icon;

  // Self-dismiss after the window for transient kinds; attention kinds
  // (approval/error) stay until the user acts or opens the session.
  useEffect(() => {
    if (cfg.sticky) return;
    const t = setTimeout(() => dismiss(toast.id), AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [toast.id, dismiss, cfg.sticky]);

  const act = () => {
    // Pane-aware, mirroring the sidebar row's open(): land in the focused pane
    // when panes are active, else switch the global active session. Jumping to the
    // session reveals the inline approval buttons / <ContinueAffordance>.
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
    <Toast className={cn("border-l-2", cfg.borderClass)}>
      <div className="flex items-start gap-2.5">
        <Icon className={cn("mt-0.5 size-4 shrink-0", cfg.iconClass)} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-medium">{toast.title}</p>
          <p className="mt-0.5 text-[12px] text-muted-foreground">
            {cfg.label}
          </p>
          <div className="mt-2">
            <Button
              size="sm"
              className={cn("h-7 gap-1 text-xs", cfg.actionClass)}
              onClick={act}
            >
              {toast.kind === "stopped" ? (
                <CornerDownLeft className="size-3" />
              ) : null}
              {cfg.action}
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
