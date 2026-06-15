import { useEffect, useRef } from "react";
import { Settings, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ThemeSettings } from "@/components/theme-settings";
import { useSettingsStore } from "@/store/settings";

/** Slide-over settings shell — theme/font today, provider settings (#8) later. */
export function SettingsPanel() {
  const open = useSettingsStore((s) => s.open);
  if (!open) return null;
  return <SettingsBody />;
}

function SettingsBody() {
  const closeSettings = useSettingsStore((s) => s.closeSettings);
  const dialogRef = useRef<HTMLElement>(null);

  // Move focus into the panel on open and restore it on close (mirrors #65).
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    return () => previouslyFocused?.focus?.();
  }, []);

  return (
    <>
      <button
        type="button"
        aria-label="Close settings"
        className="fixed inset-0 z-40 bg-background/60 backdrop-blur-[1px]"
        onClick={closeSettings}
      />
      <aside
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-label="Settings"
        className="fixed inset-y-0 right-0 z-50 flex w-80 max-w-[90vw] flex-col border-l bg-background shadow-xl outline-none"
      >
        <header className="flex h-12 shrink-0 items-center justify-between border-b px-4">
          <div className="flex items-center gap-2">
            <Settings className="size-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">Settings</h2>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="size-7"
            onClick={closeSettings}
            title="Close (Esc)"
          >
            <X className="size-4" />
          </Button>
        </header>

        <div className="flex-1 overflow-y-auto px-4 py-4">
          <ThemeSettings />

          <section className="mt-8 border-t pt-5">
            <h3 className="mb-1 text-[13px] font-medium text-foreground">
              LLM provider
            </h3>
            <p className="text-[12px] leading-relaxed text-muted-foreground">
              Provider configuration (#8) will live here.
            </p>
          </section>
        </div>
      </aside>
    </>
  );
}
