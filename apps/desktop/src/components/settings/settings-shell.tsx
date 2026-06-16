import { useEffect, useRef } from "react";
import { RotateCcw, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { SectionNav } from "@/components/settings/section-nav";
import { SettingsSectionContent } from "@/components/settings/section";
import { getSectionMeta } from "@/components/settings/registry";
import { useSettingsStore } from "@/store/settings";

/**
 * Centered settings modal (SET.1a): scrim + dialog with a two-group left nav, a
 * content pane (header + scrollable body), and a persistent footer reset slot.
 * Esc-to-close is handled by the global shortcut handler in app-shell.
 */
export function SettingsShell() {
  const closeSettings = useSettingsStore((s) => s.closeSettings);
  const activeSection = useSettingsStore((s) => s.activeSection);
  const resetHandler = useSettingsStore((s) => s.resetHandler);
  const dialogRef = useRef<HTMLDivElement>(null);

  // Move focus into the dialog on open and restore it on close (ported from the
  // old slide-over, settings-panel.tsx lines 19–24).
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    return () => previouslyFocused?.focus?.();
  }, []);

  const title = getSectionMeta(activeSection).label;

  return (
    <>
      <button
        type="button"
        aria-label="Close settings"
        className="fixed inset-0 z-40 bg-background/60 backdrop-blur-[1px]"
        onClick={closeSettings}
      />
      <div className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center p-4">
        <div
          ref={dialogRef}
          tabIndex={-1}
          role="dialog"
          aria-label="Settings"
          className="pointer-events-auto flex max-h-[85vh] w-full max-w-3xl overflow-hidden rounded-xl border bg-background shadow-xl outline-none"
        >
          <SectionNav />

          <div className="flex min-w-0 flex-1 flex-col">
            <header className="flex h-12 shrink-0 items-center justify-between border-b px-4">
              <h3 className="text-[13px] font-semibold text-foreground">
                {title}
              </h3>
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

            <ScrollArea className="flex-1">
              <div className="px-5 py-4">
                <SettingsSectionContent id={activeSection} />
              </div>
            </ScrollArea>

            <footer className="flex shrink-0 items-center justify-end border-t px-4 py-3">
              <Button
                variant="ghost"
                size="sm"
                className="gap-1.5 text-[12px]"
                disabled={!resetHandler}
                onClick={() => resetHandler?.()}
              >
                <RotateCcw className="size-3.5" />
                Reset to defaults
              </Button>
            </footer>
          </div>
        </div>
      </div>
    </>
  );
}
