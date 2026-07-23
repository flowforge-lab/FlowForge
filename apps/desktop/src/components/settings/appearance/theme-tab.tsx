import { FONTS, FONT_SCALE_MAX, FONT_SCALE_MIN } from "@/lib/fonts";
import type { Font } from "@/lib/fonts";
import type { Theme } from "@/lib/theme";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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

  const currentFont = FONTS.find((f) => f.id === font);

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
        <Select value={font} onValueChange={(v) => setFont(v as Font)}>
          <SelectTrigger aria-label="Font">
            <SelectValue placeholder="Select a font">
              <span style={{ fontFamily: currentFont?.cssValue }}>
                {currentFont?.label}
              </span>
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            {FONTS.map((f) => (
              <SelectItem key={f.id} value={f.id}>
                <span
                  className="flex items-center justify-between gap-6"
                  style={{ fontFamily: f.cssValue }}
                >
                  <span>{f.label}</span>
                  <span className="text-muted-foreground" aria-hidden>
                    Aa
                  </span>
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
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
