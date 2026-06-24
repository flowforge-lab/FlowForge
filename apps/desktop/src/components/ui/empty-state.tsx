import * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Shared empty state (#284 §3) — centered optional icon + title + optional hint,
 * replacing the bespoke one-off `<p>`s for empty session lists / empty search.
 * Matches the muted typography already used at those call sites.
 */
function EmptyState({
  icon: Icon,
  title,
  hint,
  className,
  ...props
}: React.ComponentProps<"div"> & {
  icon?: React.ComponentType<{ className?: string }>;
  title: React.ReactNode;
  hint?: React.ReactNode;
}) {
  return (
    <div
      data-slot="empty-state"
      className={cn(
        "flex flex-col items-center gap-1 px-2 py-6 text-center",
        className,
      )}
      {...props}
    >
      {Icon ? (
        <Icon className="mb-1 size-5 text-muted-foreground/50" aria-hidden />
      ) : null}
      <p className="text-[12px] text-muted-foreground/60">{title}</p>
      {hint ? (
        <p className="text-[11px] text-muted-foreground/50">{hint}</p>
      ) : null}
    </div>
  );
}

export { EmptyState };
