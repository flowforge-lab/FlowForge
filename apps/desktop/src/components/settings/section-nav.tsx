import { cn } from "@/lib/utils";
import { SETTINGS_NAV } from "@/components/settings/registry";
import { useSettingsStore } from "@/store/settings";

/** Left navigation: heading + two grouped section lists (PROFILE / GLOBAL). */
export function SectionNav() {
  const activeSection = useSettingsStore((s) => s.activeSection);
  const setSection = useSettingsStore((s) => s.setSection);

  return (
    <nav
      aria-label="Settings sections"
      className="flex w-48 shrink-0 flex-col gap-4 border-r bg-muted/30 px-3 py-4"
    >
      <h2 className="px-2 text-[13px] font-semibold text-foreground">
        Settings
      </h2>

      {SETTINGS_NAV.map((group) => (
        <div key={group.group} className="flex flex-col gap-1">
          <p className="px-2 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            {group.group}
          </p>
          {group.items.map(({ id, label, icon: Icon }) => {
            const selected = id === activeSection;
            return (
              <button
                key={id}
                type="button"
                aria-current={selected ? "page" : undefined}
                onClick={() => setSection(id)}
                className={cn(
                  "flex items-center gap-2 rounded-lg border px-2.5 py-1.5 text-left text-[13px] transition-colors",
                  selected
                    ? "border-primary bg-primary/5 text-foreground ring-2 ring-primary/30"
                    : "border-transparent text-muted-foreground hover:bg-muted/50 hover:text-foreground",
                )}
              >
                <Icon className="size-4 shrink-0" aria-hidden />
                <span className="truncate">{label}</span>
              </button>
            );
          })}
        </div>
      ))}
    </nav>
  );
}
