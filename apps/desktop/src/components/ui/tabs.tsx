import * as React from "react";
import { Tabs as TabsPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";

/**
 * Tabs primitives (radix `Tabs`) — the shared building blocks (#284). Underline
 * "segmented" look matching the settings sub-tab bar. Compose as
 * `Tabs > TabsList > TabsTrigger` + `TabsContent`. For the labeled controlled
 * wrapper used across Settings, see `settings/sub-tabs.tsx`.
 */
function Tabs({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  return (
    <TabsPrimitive.Root data-slot="tabs" className={className} {...props} />
  );
}

function TabsList({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      className={cn(
        "inline-flex items-center gap-1 border-b border-border",
        className,
      )}
      {...props}
    />
  );
}

function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      data-slot="tabs-trigger"
      className={cn(
        "-mb-px border-b-2 border-transparent px-3 py-1.5 text-[13px] font-medium transition-colors outline-none",
        "text-muted-foreground hover:text-foreground",
        "focus-visible:ring-2 focus-visible:ring-primary/30",
        "data-[state=active]:border-primary data-[state=active]:text-foreground",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...props}
    />
  );
}

function TabsContent({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      className={cn("mt-5 outline-none", className)}
      {...props}
    />
  );
}

export { Tabs, TabsList, TabsTrigger, TabsContent };
