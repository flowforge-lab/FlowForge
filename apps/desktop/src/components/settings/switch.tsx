import { cn } from "@/lib/utils";
import { Switch } from "@/components/ui/switch";

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
 * Labeled on/off toggle: a label + optional description row wrapping the shared
 * `ui/switch` primitive (#284). Controlled — `checked` in, `onCheckedChange` out.
 * Keyboard-operable (Space/Enter) and disabled-aware.
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
      <Switch
        aria-label={label}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
      />
    </label>
  );
}
