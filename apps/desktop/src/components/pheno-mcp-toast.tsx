import { useEffect, useMemo } from "react";
import { PlugZap, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Toast, ToastViewport } from "@/components/ui/toast";
import {
  unavailableServerDetails,
  unavailableToastBody,
} from "@/lib/phenotype-mcp";
import { usePhenoMcpNoticeStore } from "@/store/pheno-mcp-notice";
import { useMcpStore } from "@/store/mcp";
import { useSettingsStore } from "@/store/settings";

// Sticky notice shown when a just-activated phenotype lists a skill whose declared
// MCP server is unavailable (#301/#573). Persistent by design: a codegraph-dependent
// phenotype that silently degrades to grep gives no actionable signal, so this does
// NOT auto-dismiss (an auto-dismissing toast races activation and vanishes before the
// user notices — #573 2c). It stays until the user dismisses it or every named server
// recovers to `running`. It surfaces the actual spawn/connect error per server (from
// the live MCP status snapshot) and a one-click shortcut to the MCP settings panel.
// Activation never blocks; the skill's grep/glob fallbacks still work.

export function PhenoMcpToast() {
  const notice = usePhenoMcpNoticeStore((s) => s.notice);
  const dismiss = usePhenoMcpNoticeStore((s) => s.dismiss);
  const servers = useMcpStore((s) => s.servers);
  const openSettings = useSettingsStore((s) => s.openSettings);
  const setSection = useSettingsStore((s) => s.setSection);

  const statusById = useMemo(
    () => new Map(servers.map((s) => [s.id, s])),
    [servers],
  );

  // Auto-clear once every named server is running again, so the notice is loud while
  // broken but does not linger after the user repairs the server (e.g. fixes the
  // command or starts it from MCP settings). Nothing else clears it.
  useEffect(() => {
    if (!notice) return;
    const allRunning = notice.servers.every(
      (id) => statusById.get(id)?.state === "running",
    );
    if (allRunning) dismiss();
  }, [notice, statusById, dismiss]);

  if (!notice) return null;

  const details = unavailableServerDetails(notice, statusById);

  const openMcpSettings = () => {
    openSettings();
    setSection("mcp");
    dismiss();
  };

  return (
    // Viewport is click-through so it never blocks the canvas behind it; the
    // card re-enables pointer events.
    <ToastViewport>
      <Toast>
        <div className="flex items-start gap-2.5">
          <PlugZap className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <p className="text-[13px] font-medium">MCP tools unavailable</p>
            <p className="mt-0.5 text-[12px] text-muted-foreground">
              {unavailableToastBody(notice)}
            </p>
            {details.length > 0 ? (
              <ul className="mt-1.5 space-y-0.5">
                {details.map(({ server, detail }) => (
                  <li
                    key={server}
                    className="text-[11px] leading-relaxed text-destructive"
                  >
                    <span className="font-medium">{server}:</span> {detail}
                  </li>
                ))}
              </ul>
            ) : null}
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
      </Toast>
    </ToastViewport>
  );
}
