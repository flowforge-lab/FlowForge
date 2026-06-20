import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

// Compact status pill (#284) — replaces the hand-rolled trust/safety badges. The
// base matches the app's existing badge look (tiny uppercase, tinted); `tone`
// picks the color. Use `tone` for status color, not a semantic variant, since
// these badges encode trust/safety levels rather than button-like intents.
const badgeVariants = cva(
  "inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide whitespace-nowrap",
  {
    variants: {
      tone: {
        neutral: "bg-muted text-muted-foreground",
        amber: "bg-amber-500/15 text-amber-700 dark:text-amber-400",
        emerald: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400",
        sky: "bg-sky-500/15 text-sky-700 dark:text-sky-400",
        destructive: "bg-destructive/15 text-destructive",
      },
    },
    defaultVariants: {
      tone: "neutral",
    },
  },
);

function Badge({
  className,
  tone,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return (
    <span
      data-slot="badge"
      className={cn(badgeVariants({ tone, className }))}
      {...props}
    />
  );
}

export { Badge };
