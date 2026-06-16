import { Switch as SwitchPrimitive } from "radix-ui";
import { cn } from "@/lib/utils";

interface SettingsSwitchProps {
  /** Accessible name for the toggle (also rendered as the row label). */
  label: string;
  /** Optional helper text under the label. */
  description?: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  className?: string;
}

/**
 * Labeled on/off toggle (radix `Switch`). Controlled — `checked` in,
 * `onCheckedChange` out. Keyboard-operable (Space/Enter) and disabled-aware.
 */
export function SettingsSwitch({
  label,
  description,
  checked,
  onCheckedChange,
  disabled,
  className,
}: SettingsSwitchProps) {
  return (
    <label
      className={cn(
        "flex items-center justify-between gap-4",
        disabled && "opacity-50",
        className,
      )}
    >
      <span className="flex flex-col gap-0.5">
        <span className="text-[13px] font-medium text-foreground">{label}</span>
        {description ? (
          <span className="text-[12px] leading-relaxed text-muted-foreground">
            {description}
          </span>
        ) : null}
      </span>
      <SwitchPrimitive.Root
        aria-label={label}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        className={cn(
          "peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-border bg-muted transition-colors outline-none",
          "focus-visible:ring-2 focus-visible:ring-primary/30",
          "data-[state=checked]:border-primary data-[state=checked]:bg-primary",
          "disabled:cursor-not-allowed",
        )}
      >
        <SwitchPrimitive.Thumb
          className={cn(
            "pointer-events-none block size-4 translate-x-0.5 rounded-full bg-background shadow-sm transition-transform",
            "data-[state=checked]:translate-x-[18px]",
          )}
        />
      </SwitchPrimitive.Root>
    </label>
  );
}
