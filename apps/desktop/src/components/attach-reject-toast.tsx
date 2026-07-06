// Rejection notices for attachment drops (#723). When a drop / paste / pick
// rejects files (unsupported type, or a kind the model can't accept), the shared
// staging path enqueues a message here; each card states the reason and
// auto-dismisses. Mirrors <SessionDoneToast>: the store owns the queue, this
// renders it; auto-dismiss is owned per-card, matching the ui/toast.tsx contract.

import { useEffect } from "react";
import { AlertTriangle, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Toast, ToastViewport } from "@/components/ui/toast";
import {
  useAttachRejectToastStore,
  type RejectToast,
} from "@/store/attach-reject-toast";

const AUTO_DISMISS_MS = 6_000;

export function AttachRejectToast() {
  const toasts = useAttachRejectToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;

  return (
    // Bottom-left — clears the bottom-right MCP notice and the top-right
    // completion toasts so the three never overlap. `right-auto` unsets the
    // viewport default.
    <ToastViewport className="left-4 right-auto">
      <div className="flex flex-col gap-2">
        {toasts.map((toast) => (
          <AttachRejectToastCard key={toast.id} toast={toast} />
        ))}
      </div>
    </ToastViewport>
  );
}

function AttachRejectToastCard({ toast }: { toast: RejectToast }) {
  const dismiss = useAttachRejectToastStore((s) => s.dismiss);

  useEffect(() => {
    const t = setTimeout(() => dismiss(toast.id), AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [toast.id, dismiss]);

  return (
    <Toast>
      <div className="flex items-start gap-2.5">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-500" />
        <p className="min-w-0 flex-1 text-[13px]">{toast.message}</p>
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
