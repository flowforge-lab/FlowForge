import * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Non-blocking toast (#284 §2) — promoted from the app's existing hand-rolled
 * pattern (the phenotype-MCP notice). No new dependency: a thin presentational
 * pair the existing store-driven toasts compose, so the surface/positioning is
 * defined in one place.
 *
 * `ToastViewport` is the click-through fixed anchor (bottom-right, one slot);
 * `Toast` is the announced card (`role="status"` + `aria-live="polite"`).
 * Behavior (auto-dismiss, pause-on-hover) stays with the caller.
 */
function ToastViewport({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="toast-viewport"
      className={cn(
        "pointer-events-none fixed right-4 bottom-4 z-50 flex max-w-sm",
        className,
      )}
      {...props}
    />
  );
}

function Toast({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="toast"
      role="status"
      aria-live="polite"
      className={cn(
        "pointer-events-auto w-full rounded-lg border bg-popover/95 p-3 text-popover-foreground shadow-lg backdrop-blur",
        className,
      )}
      {...props}
    />
  );
}

export { Toast, ToastViewport };
