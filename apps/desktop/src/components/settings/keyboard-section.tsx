import { useEffect } from "react";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { useSettingsStore } from "@/store/settings";
import { usePrefsStore, type SendMessageKey } from "@/store/prefs";
import { keyboardReferenceGroups } from "@/lib/keyboard-reference";
import { MODE_META } from "@/lib/mode";
import type { Mode } from "@/bindings";

// "Mod" renders as the platform's primary modifier. Kept here (browser-only) so
// lib/shortcuts.ts stays platform-pure and node-testable — mirrors the same
// helper in shortcuts-overlay.tsx.
const IS_MAC =
  typeof navigator !== "undefined" &&
  /mac/i.test(navigator.platform || navigator.userAgent || "");

function renderKey(key: string): string {
  return key === "Mod" ? (IS_MAC ? "⌘" : "Ctrl") : key;
}

const SEND_OPTIONS: ReadonlyArray<{ value: SendMessageKey; label: string }> = [
  { value: "enter", label: "Enter" },
  { value: "ctrlEnter", label: "Ctrl/⌘+Enter" },
];

const MODE_OPTIONS: ReadonlyArray<{ value: Mode; label: string }> = (
  ["plan", "act", "auto"] as const
).map((m) => ({ value: m, label: MODE_META[m].label }));

/**
 * Keyboard section (#129, SET.6): a read-only shortcut reference plus the editable
 * **Send message** binding. Data comes from the single `lib/shortcuts.ts` registry
 * (same source as the ⌘/ help overlay); the footer reset restores the binding.
 */
export function KeyboardSection() {
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);
  const resetKeyboard = usePrefsStore((s) => s.resetKeyboard);
  const sendMessageKey = usePrefsStore((s) => s.sendMessageKey);
  const setSendMessageKey = usePrefsStore((s) => s.setSendMessageKey);
  const defaultMode = usePrefsStore((s) => s.defaultMode);
  const setDefaultMode = usePrefsStore((s) => s.setDefaultMode);

  useEffect(() => {
    registerResetHandler(resetKeyboard);
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetKeyboard]);

  const reference = keyboardReferenceGroups(sendMessageKey);

  return (
    <div className="space-y-6">
      <section className="space-y-2">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground/60">
          Preferences
        </div>
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <p className="text-[13px] text-foreground">Send message</p>
            <p className="text-[11px] text-muted-foreground">
              {sendMessageKey === "ctrlEnter"
                ? "Enter inserts a new line."
                : "Shift+Enter inserts a new line."}
            </p>
          </div>
          <SegmentedControl
            label="Send message key"
            options={SEND_OPTIONS}
            value={sendMessageKey}
            onValueChange={setSendMessageKey}
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <p className="text-[13px] text-foreground">Default mode</p>
            <p className="text-[11px] text-muted-foreground">
              {MODE_META[defaultMode].description} New sessions start here.
            </p>
          </div>
          <SegmentedControl
            label="Default agent mode"
            options={MODE_OPTIONS}
            value={defaultMode}
            onValueChange={setDefaultMode}
          />
        </div>
      </section>

      {reference.map(({ group, items }) => (
        <section key={group} className="space-y-2">
          <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground/60">
            {group}
          </div>
          <ul className="flex flex-col gap-1.5">
            {items.map((s) => (
              <li
                key={`${s.group}:${s.label}`}
                className="flex items-center justify-between gap-4 text-[13px]"
              >
                <span className="min-w-0 text-foreground/90">{s.label}</span>
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
        </section>
      ))}
    </div>
  );
}
