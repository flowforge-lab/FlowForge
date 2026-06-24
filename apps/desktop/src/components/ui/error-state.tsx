import * as React from "react";
import { AlertTriangle } from "@/components/ui/icon";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

/**
 * Shared inline error state (#284 §3) — destructive icon + message with an
 * optional retry affordance, consolidating the duplicated marketplace-style
 * error blocks. `role="alert"` so it's announced. The "Try again" button renders
 * only when `onRetry` is provided.
 */
function ErrorState({
  title,
  message,
  onRetry,
  retryLabel = "Try again",
  className,
  ...props
}: Omit<React.ComponentProps<"div">, "title"> & {
  title?: React.ReactNode;
  message: React.ReactNode;
  onRetry?: () => void;
  retryLabel?: string;
}) {
  return (
    <div
      data-slot="error-state"
      role="alert"
      className={cn("flex flex-col items-start gap-2", className)}
      {...props}
    >
      <p className="flex items-center gap-2 text-[12px] text-destructive">
        <AlertTriangle className="size-4 shrink-0" aria-hidden />
        <span>
          {title ? <strong className="font-semibold">{title}</strong> : null}
          {title ? " " : null}
          {message}
        </span>
      </p>
      {onRetry ? (
        <Button type="button" variant="outline" size="xs" onClick={onRetry}>
          {retryLabel}
        </Button>
      ) : null}
    </div>
  );
}

export { ErrorState };
