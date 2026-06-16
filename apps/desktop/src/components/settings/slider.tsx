import { Slider as SliderPrimitive } from "radix-ui";
import { cn } from "@/lib/utils";

interface SettingsSliderProps {
  label: string;
  value: number;
  onValueChange: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  /** Formats the readout next to the label (defaults to the raw value). */
  formatValue?: (value: number) => string;
  className?: string;
}

/**
 * Labeled range with a value readout (radix `Slider`). Controlled single value —
 * `value` in, `onValueChange` out. Keyboard-operable (arrows/Home/End) and
 * disabled-aware.
 */
export function SettingsSlider({
  label,
  value,
  onValueChange,
  min = 0,
  max = 100,
  step = 1,
  disabled,
  formatValue,
  className,
}: SettingsSliderProps) {
  return (
    <div className={cn("space-y-2", disabled && "opacity-50", className)}>
      <div className="flex items-center justify-between gap-4">
        <span className="text-[13px] font-medium text-foreground">{label}</span>
        <span className="text-[12px] tabular-nums text-muted-foreground">
          {formatValue ? formatValue(value) : value}
        </span>
      </div>
      <SliderPrimitive.Root
        aria-label={label}
        value={[value]}
        onValueChange={([next]) => onValueChange(next)}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        className="relative flex h-4 w-full touch-none items-center select-none data-[disabled]:cursor-not-allowed"
      >
        <SliderPrimitive.Track className="relative h-1.5 w-full grow rounded-full bg-muted">
          <SliderPrimitive.Range className="absolute h-full rounded-full bg-primary" />
        </SliderPrimitive.Track>
        <SliderPrimitive.Thumb
          className={cn(
            "block size-4 rounded-full border border-primary bg-background shadow-sm transition-colors outline-none",
            "focus-visible:ring-2 focus-visible:ring-primary/30",
          )}
        />
      </SliderPrimitive.Root>
    </div>
  );
}
