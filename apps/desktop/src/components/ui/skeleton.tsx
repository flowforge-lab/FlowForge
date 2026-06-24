import * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Loading placeholder (#284 §2) — a pulsing block used to reserve layout while
 * content loads, replacing the hand-rolled `animate-pulse` divs (e.g. the
 * marketplace skeletons). Decorative: callers wrap groups in `aria-hidden`.
 */
function Skeleton({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="skeleton"
      className={cn("animate-pulse rounded bg-muted", className)}
      {...props}
    />
  );
}

export { Skeleton };
