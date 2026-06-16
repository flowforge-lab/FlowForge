import type { ReactNode } from "react";
import { Tabs as TabsPrimitive } from "radix-ui";
import { cn } from "@/lib/utils";

export interface SubTab<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
}

interface SubTabsProps<T extends string> {
  /** Accessible name for the tab list. */
  label: string;
  tabs: ReadonlyArray<SubTab<T>>;
  value: T;
  onValueChange: (value: T) => void;
  /** The active tab's pane. Rendered inside `Tabs.Content` so the tab/panel
   *  a11y wiring (role="tabpanel" + aria-labelledby, resolved aria-controls) is
   *  preserved — pass it rather than rendering the pane outside `SubTabs`. */
  children?: ReactNode;
  className?: string;
}

/**
 * Horizontal segmented sub-tab bar (radix `Tabs`, e.g. Theme | Notifications |
 * Advanced). Controlled, keyboard-operable (arrow keys), role=tablist.
 *
 * Pass the active pane as `children`: it's rendered in a `Tabs.Content` keyed to
 * the active `value`, which keeps radix's tab↔panel wiring intact (the active
 * trigger's `aria-controls` resolves and the panel is labelled by its trigger).
 * Omit `children` only for a pure bar with no panel.
 */
export function SubTabs<T extends string>({
  label,
  tabs,
  value,
  onValueChange,
  children,
  className,
}: SubTabsProps<T>) {
  return (
    <TabsPrimitive.Root
      value={value}
      onValueChange={(next) => onValueChange(next as T)}
    >
      <TabsPrimitive.List
        aria-label={label}
        className={cn(
          "inline-flex items-center gap-1 border-b border-border",
          className,
        )}
      >
        {tabs.map((tab) => (
          <TabsPrimitive.Trigger
            key={tab.value}
            value={tab.value}
            disabled={tab.disabled}
            className={cn(
              "-mb-px border-b-2 border-transparent px-3 py-1.5 text-[13px] font-medium transition-colors outline-none",
              "text-muted-foreground hover:text-foreground",
              "focus-visible:ring-2 focus-visible:ring-primary/30",
              "data-[state=active]:border-primary data-[state=active]:text-foreground",
              "disabled:cursor-not-allowed disabled:opacity-50",
            )}
          >
            {tab.label}
          </TabsPrimitive.Trigger>
        ))}
      </TabsPrimitive.List>

      {children !== undefined ? (
        // `value` is always the active tab, so this panel is the active one;
        // radix gives it role="tabpanel" + aria-labelledby and makes the active
        // trigger's aria-controls resolve to it.
        <TabsPrimitive.Content value={value} className="mt-5 outline-none">
          {children}
        </TabsPrimitive.Content>
      ) : null}
    </TabsPrimitive.Root>
  );
}
