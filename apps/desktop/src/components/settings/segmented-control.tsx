import { ToggleGroup as ToggleGroupPrimitive } from "radix-ui";
import { cn } from "@/lib/utils";

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
}

interface SegmentedControlProps<T extends string> {
  /** Accessible name for the group. */
  label: string;
  options: ReadonlyArray<SegmentedOption<T>>;
  value: T;
  onValueChange: (value: T) => void;
  disabled?: boolean;
  className?: string;
}

/**
 * Generic single-select pill row (radix `ToggleGroup`, type single). Extracts the
 * selected-state pattern used inline in `theme-settings.tsx`. Keyboard-operable
 * (arrow keys roving focus) and disabled-aware.
 */
export function SegmentedControl<T extends string>({
  label,
  options,
  value,
  onValueChange,
  disabled,
  className,
}: SegmentedControlProps<T>) {
  return (
    <ToggleGroupPrimitive.Root
      type="single"
      aria-label={label}
      value={value}
      // Radix emits "" when the active item is toggled off; ignore that so the
      // control stays single-select (one option always active).
      onValueChange={(next) => {
        if (next) onValueChange(next as T);
      }}
      disabled={disabled}
      className={cn(
        "inline-flex items-center gap-1 rounded-lg border border-border bg-muted/50 p-1",
        disabled && "opacity-50",
        className,
      )}
    >
      {options.map((option) => (
        <ToggleGroupPrimitive.Item
          key={option.value}
          value={option.value}
          disabled={option.disabled}
          className={cn(
            "rounded-md border border-transparent px-3 py-1 text-[12px] font-medium transition-colors outline-none",
            "text-muted-foreground hover:text-foreground",
            "focus-visible:ring-2 focus-visible:ring-primary/30",
            "data-[state=on]:border-primary data-[state=on]:bg-primary/5 data-[state=on]:text-foreground data-[state=on]:ring-2 data-[state=on]:ring-primary/30",
            "disabled:cursor-not-allowed disabled:opacity-50",
          )}
        >
          {option.label}
        </ToggleGroupPrimitive.Item>
      ))}
    </ToggleGroupPrimitive.Root>
  );
}
