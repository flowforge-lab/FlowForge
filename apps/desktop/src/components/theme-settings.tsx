import { cn } from "@/lib/utils";
import { FONTS } from "@/lib/fonts";
import { THEMES } from "@/lib/theme";
import { usePrefsStore } from "@/store/prefs";

function ThemeCard({
  label,
  previewBg,
  selected,
  onSelect,
}: {
  label: string;
  previewBg: string;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onSelect}
      className={cn(
        "flex flex-col items-center gap-2 rounded-lg border px-3 py-2.5 text-left transition-colors",
        selected
          ? "border-primary bg-primary/5 ring-2 ring-primary/30"
          : "border-border bg-card hover:bg-muted/50",
      )}
    >
      <span
        className="size-8 rounded-full border border-border/60 shadow-inner"
        style={{ background: previewBg }}
        aria-hidden
      />
      <span className="text-[12px] font-medium text-foreground">{label}</span>
    </button>
  );
}

function FontOption({
  label,
  cssValue,
  selected,
  onSelect,
}: {
  label: string;
  cssValue: string;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onSelect}
      className={cn(
        "flex items-center justify-between gap-3 rounded-lg border px-3 py-2.5 transition-colors",
        selected
          ? "border-primary bg-primary/5 ring-2 ring-primary/30"
          : "border-border bg-card hover:bg-muted/50",
      )}
    >
      <span className="text-[12px] font-medium text-foreground">{label}</span>
      <span
        className="text-lg leading-none text-muted-foreground"
        style={{ fontFamily: cssValue }}
        aria-hidden
      >
        Aa
      </span>
    </button>
  );
}

/** Theme + font pickers — live-apply via `usePrefsStore`, no Apply button. */
export function ThemeSettings() {
  const theme = usePrefsStore((s) => s.theme);
  const font = usePrefsStore((s) => s.font);
  const setTheme = usePrefsStore((s) => s.setTheme);
  const setFont = usePrefsStore((s) => s.setFont);

  return (
    <div className="space-y-5">
      <section>
        <h3 className="mb-2 text-[13px] font-medium text-foreground">
          Appearance
        </h3>
        <div className="grid grid-cols-3 gap-2">
          {THEMES.map((t) => (
            <ThemeCard
              key={t.id}
              label={t.label}
              previewBg={t.previewBg}
              selected={theme === t.id}
              onSelect={() => setTheme(t.id)}
            />
          ))}
        </div>
      </section>

      <section>
        <h3 className="mb-2 text-[13px] font-medium text-foreground">Font</h3>
        <div className="flex flex-col gap-2">
          {FONTS.map((f) => (
            <FontOption
              key={f.id}
              label={f.label}
              cssValue={f.cssValue}
              selected={font === f.id}
              onSelect={() => setFont(f.id)}
            />
          ))}
        </div>
      </section>
    </div>
  );
}
