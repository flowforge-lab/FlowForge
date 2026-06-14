import { useEffect, useRef } from "react";
import { useShortcutsStore } from "@/store/shortcuts";
import { groupedShortcuts } from "@/lib/shortcuts";

// "Mod" renders as the platform's primary modifier. Kept here (browser-only) so
// the registry in lib/shortcuts.ts stays platform-pure and testable in node.
const IS_MAC =
  typeof navigator !== "undefined" &&
  /mac/i.test(navigator.platform || navigator.userAgent || "");

function renderKey(key: string): string {
  return key === "Mod" ? (IS_MAC ? "⌘" : "Ctrl") : key;
}

// Thin wrapper so the body mounts fresh on open (and costs nothing while closed).
export function ShortcutsOverlay() {
  const open = useShortcutsStore((s) => s.open);
  if (!open) return null;
  return <ShortcutsBody />;
}

function ShortcutsBody() {
  const close = useShortcutsStore((s) => s.closeShortcuts);
  const dialogRef = useRef<HTMLDivElement>(null);

  // Move focus into the dialog on open and restore it on close. No focus trap —
  // Esc (app-shell) and click-outside close; this just keeps focus sensible.
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    return () => previouslyFocused?.focus?.();
  }, []);

  const groups = groupedShortcuts();

  return (
    <div
      role="dialog"
      // Intentionally not aria-modal: the overlay doesn't trap focus (Issue #20
      // — "no modal-trap"), so claiming modality would over-promise to AT.
      aria-label="Keyboard shortcuts"
      className="fixed inset-0 z-50 flex items-start justify-center"
    >
      {/* Click-outside closes; separate element so a click on the panel never
          reaches it. */}
      <div
        className="absolute inset-0 bg-background/60 backdrop-blur-sm"
        onMouseDown={close}
      />

      <div
        ref={dialogRef}
        tabIndex={-1}
        className="relative mt-[12vh] flex max-h-[70vh] w-[92%] max-w-md flex-col overflow-hidden rounded-xl border bg-card shadow-2xl outline-none"
      >
        <div className="flex shrink-0 items-center justify-between border-b px-4 py-2.5">
          <span className="text-[13px] font-medium text-foreground">
            Keyboard shortcuts
          </span>
          <kbd className="font-mono text-[11px] text-muted-foreground/60">
            ?
          </kbd>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {groups.map(({ group, items }) => (
            <div key={group} className="mb-3 last:mb-0">
              <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground/60">
                {group}
              </div>
              <ul className="flex flex-col gap-1.5">
                {items.map((s) => (
                  <li
                    key={`${s.group}:${s.label}`}
                    className="flex items-center justify-between gap-4 text-[13px]"
                  >
                    <span className="min-w-0 text-foreground/90">
                      {s.label}
                    </span>
                    <span className="flex shrink-0 items-center gap-1">
                      {s.keys.map((key, i) =>
                        key === "or" ? (
                          <span key={i} className="text-muted-foreground/50">
                            or
                          </span>
                        ) : (
                          <kbd
                            key={i}
                            className="rounded border bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                          >
                            {renderKey(key)}
                          </kbd>
                        ),
                      )}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>

        <div className="flex shrink-0 items-center justify-end gap-1 border-t px-4 py-2 text-[11px] text-muted-foreground/60">
          <kbd className="font-mono">Esc</kbd>
          close
        </div>
      </div>
    </div>
  );
}
