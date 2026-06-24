import * as React from "react";
import { Loader2 } from "@/components/ui/icon";

import { cn } from "@/lib/utils";

/**
 * Inline loading spinner (#284 §3) — the single shared `Loader2 + animate-spin`
 * primitive, replacing the per-component copies. Renders `role="status"` with an
 * accessible label (the hand-rolled spinners had none). `size` covers the two
 * sizes used across the app (`sm` = the common inline `size-3.5`, `md` = `size-4`);
 * color is inherited (pass `text-muted-foreground` via `className` where wanted).
 */
function Spinner({
  size = "sm",
  className,
  "aria-label": label = "Loading",
  ...props
}: React.ComponentProps<typeof Loader2> & { size?: "sm" | "md" }) {
  return (
    <Loader2
      data-slot="spinner"
      role="status"
      aria-label={label}
      className={cn(
        "animate-spin",
        size === "sm" ? "size-3.5" : "size-4",
        className,
      )}
      {...props}
    />
  );
}

export { Spinner };
