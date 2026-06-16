import { cn } from "@/lib/utils";
import { FONTS, FONT_SCALE_MAX, FONT_SCALE_MIN } from "@/lib/fonts";
import type { Theme } from "@/lib/theme";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { SettingsSlider } from "@/components/settings/slider";
import { usePrefsStore } from "@/store/prefs";

const MODE_OPTIONS: ReadonlyArray<{ value: Theme; label: string }> = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "System" },
];

/** Theme sub-tab: mode, font, font size, and display name. All live-apply. */
export function ThemeTab() {
  const theme = usePrefsStore((s) => s.theme);
  const font = usePrefsStore((s) => s.font);
  const fontScale = usePrefsStore((s) => s.fontScale);
  const displayName = usePrefsStore((s) => s.displayName);
  const setTheme = usePrefsStore((s) => s.setTheme);
  const setFont = usePrefsStore((s) => s.setFont);
  const setFontScale = usePrefsStore((s) => s.setFontScale);
  const setDisplayName = usePrefsStore((s) => s.setDisplayName);

  return (
    <div className="space-y-6">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">Mode</h3>
        <SegmentedControl
          label="Theme mode"
          options={MODE_OPTIONS}
          value={theme}
          onValueChange={setTheme}
        />
      </section>

      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">Font</h3>
        <div className="flex flex-col gap-2">
          {FONTS.map((f) => (
            <button
              key={f.id}
              type="button"
              aria-pressed={font === f.id}
              onClick={() => setFont(f.id)}
              className={cn(
                "flex items-center justify-between gap-3 rounded-lg border px-3 py-2.5 transition-colors",
                font === f.id
                  ? "border-primary bg-primary/5 ring-2 ring-primary/30"
                  : "border-border bg-card hover:bg-muted/50",
              )}
            >
              <span className="text-[12px] font-medium text-foreground">
                {f.label}
              </span>
              <span
                className="text-lg leading-none text-muted-foreground"
                style={{ fontFamily: f.cssValue }}
                aria-hidden
              >
                Aa
              </span>
            </button>
          ))}
        </div>
      </section>

      <section>
        <SettingsSlider
          label="Font size"
          value={fontScale}
          onValueChange={setFontScale}
          min={FONT_SCALE_MIN}
          max={FONT_SCALE_MAX}
          step={10}
          formatValue={(v) => `${v}%`}
        />
      </section>

      <section className="space-y-1.5">
        <label
          htmlFor="appearance-display-name"
          className="text-[13px] font-medium text-foreground"
        >
          Display name
        </label>
        <Input
          id="appearance-display-name"
          value={displayName}
          placeholder="System alias"
          onChange={(e) => setDisplayName(e.target.value)}
        />
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          Overrides the name shown on messages you send. Leave blank to use the
          system alias.
        </p>
      </section>
    </div>
  );
}
