import * as React from "react";
import { Progress as ProgressPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";

/**
 * Determinate progress bar (radix `Progress`) — shared primitive (#284),
 * groundwork for the #282 context-usage gauge. Radix sets `role="progressbar"`
 * plus `aria-valuenow/min/max`, so it's accessible by default. `value` is a
 * 0–100 percentage; `null`/omitted renders the indeterminate (empty) track.
 */
function Progress({
  className,
  value,
  ...props
}: React.ComponentProps<typeof ProgressPrimitive.Root>) {
  // Clamp a numeric value to 0–100 so it drives BOTH the visual transform and the
  // value forwarded to Radix `Root` (which sets `aria-valuenow`) — otherwise an
  // out-of-range `value` leaks an invalid `aria-valuenow`. `null`/omitted passes
  // through unchanged so the indeterminate track keeps no `aria-valuenow`.
  const clamped =
    typeof value === "number" ? Math.min(100, Math.max(0, value)) : value;
  const pct = clamped ?? 0;
  return (
    <ProgressPrimitive.Root
      data-slot="progress"
      value={clamped}
      className={cn(
        "relative h-2 w-full overflow-hidden rounded-full bg-muted",
        className,
      )}
      {...props}
    >
      <ProgressPrimitive.Indicator
        data-slot="progress-indicator"
        className="h-full w-full flex-1 bg-primary transition-transform"
        style={{ transform: `translateX(-${100 - pct}%)` }}
      />
    </ProgressPrimitive.Root>
  );
}

export { Progress };
