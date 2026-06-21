import { useEffect } from "react";
import { PlugZap, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { unavailableToastBody } from "@/lib/phenotype-mcp";
import { usePhenoMcpNoticeStore } from "@/store/pheno-mcp-notice";
import { useSettingsStore } from "@/store/settings";

// Non-blocking toast shown when a just-activated phenotype lists a skill whose
// declared MCP server is unavailable (#301) — the FE surface over PR #296's
// warn-only signal. Calm by design: one slot, bottom-right, auto-dismissing, and
// purely informational (activation never blocks; the skill's grep/glob fallbacks
// still work). Offers a shortcut to the MCP settings panel to add/repair servers.

// Long enough to read and act on, short enough not to linger. Cleared early on
// dismiss or when a newer notice replaces it.
const AUTO_DISMISS_MS = 12_000;

export function PhenoMcpToast() {
  const notice = usePhenoMcpNoticeStore((s) => s.notice);
  const seq = usePhenoMcpNoticeStore((s) => s.seq);
  const dismiss = usePhenoMcpNoticeStore((s) => s.dismiss);
  const openSettings = useSettingsStore((s) => s.openSettings);
  const setSection = useSettingsStore((s) => s.setSection);

  // Re-arm on every show (`seq`), not just when a notice first appears, so a
  // replacement notice resets the countdown.
  useEffect(() => {
    if (!notice) return;
    const handle = setTimeout(dismiss, AUTO_DISMISS_MS);
    return () => clearTimeout(handle);
  }, [seq, notice, dismiss]);

  if (!notice) return null;

  const openMcpSettings = () => {
    openSettings();
    setSection("mcp");
    dismiss();
  };

  return (
    // Wrapper is click-through so it never blocks the canvas behind it; the card
    // re-enables pointer events.
    <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex max-w-sm">
      <div
        role="status"
        aria-live="polite"
        className="pointer-events-auto w-full rounded-lg border bg-popover/95 p-3 text-popover-foreground shadow-lg backdrop-blur"
      >
        <div className="flex items-start gap-2.5">
          <PlugZap className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <p className="text-[13px] font-medium">MCP tools unavailable</p>
            <p className="mt-0.5 text-[12px] text-muted-foreground">
              {unavailableToastBody(notice)}
            </p>
            <div className="mt-2">
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={openMcpSettings}
              >
                Open MCP settings
              </Button>
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="-mr-1 -mt-1 size-6 shrink-0 text-muted-foreground hover:text-foreground"
            onClick={dismiss}
            title="Dismiss"
            aria-label="Dismiss"
          >
            <X className="size-3.5" />
          </Button>
        </div>
      </div>
    </div>
  );
}
