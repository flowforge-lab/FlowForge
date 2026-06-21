import type { ReactNode } from "react";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";

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
 * Horizontal segmented sub-tab bar (e.g. Theme | Notifications | Advanced),
 * composing the shared `ui/tabs` primitives (#284). Controlled, keyboard-operable
 * (arrow keys), role=tablist.
 *
 * Pass the active pane as `children`: it's rendered in a `TabsContent` keyed to
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
    <Tabs value={value} onValueChange={(next) => onValueChange(next as T)}>
      <TabsList aria-label={label} className={className}>
        {tabs.map((tab) => (
          <TabsTrigger
            key={tab.value}
            value={tab.value}
            disabled={tab.disabled}
          >
            {tab.label}
          </TabsTrigger>
        ))}
      </TabsList>

      {children !== undefined ? (
        // `value` is always the active tab, so this panel is the active one;
        // radix gives it role="tabpanel" + aria-labelledby and makes the active
        // trigger's aria-controls resolve to it.
        <TabsContent value={value}>{children}</TabsContent>
      ) : null}
    </Tabs>
  );
}
