import { Download, Loader2, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  progressPercent,
  activeUpdateChannel,
  useUpdateStore,
} from "@/store/update";

/**
 * Global update-available banner (#565, RFC 0014 §12.1, P5a). Renders a
 * full-width bar above the app body when an update is available, with an
 * "Update" button that delegates to `useUpdateStore.install()` and a
 * dismiss (×) button that hides the bar for the current session.
 *
 * While installing, a download-progress strip (#566, §12.2) spans the bottom
 * edge: determinate when a content length is known, indeterminate (pulsing)
 * otherwise. Mirrors the Settings → About surface, which shares the same store.
 *
 * Not persisted — re-appears on relaunch if the store still reports
 * `available`. Settings → About remains the manual/debug surface.
 */
export function UpdateBar() {
  const status = useUpdateStore((s) => s.status);
  const installing = useUpdateStore((s) => s.installing);
  const dismissed = useUpdateStore((s) => s.dismissed);
  const progress = useUpdateStore((s) => s.progress);
  const install = useUpdateStore((s) => s.install);
  const dismiss = useUpdateStore((s) => s.dismiss);

  const available = status?.kind === "available";
  const show = available && !dismissed;
  const version = available ? status.version : "";
  const percent = progressPercent(progress);

  if (!show) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      className="update-bar shrink-0 border-b border-amber-400/30 bg-amber-50/80 text-amber-800 dark:border-amber-600/30 dark:bg-amber-950/40 dark:text-amber-200"
    >
      <div className="flex items-center justify-between gap-3 px-4 py-2 text-[13px]">
        <span className="flex items-center gap-1.5">
          <Download className="size-4 shrink-0" />
          <span>
            <strong>FlowForge {version}</strong> is available
          </span>
        </span>
        <div className="flex items-center gap-2">
          <Button
            size="xs"
            variant="secondary"
            onClick={() => void install(activeUpdateChannel())}
            disabled={installing}
            data-icon="inline-end"
          >
            {installing ? (
              <>
                <Loader2 className="size-3.5 shrink-0 animate-spin" /> Updating…
              </>
            ) : (
              "Update"
            )}
          </Button>
          <Button
            size="icon-xs"
            variant="ghost"
            className="shrink-0 text-current/60 hover:text-current"
            onClick={() => dismiss()}
            aria-label="Dismiss update notification"
            title="Dismiss"
          >
            <X className="size-3.5" />
          </Button>
        </div>
      </div>
      {installing ? (
        <div className="px-4 pb-2">
          <Progress
            value={percent}
            className={percent == null ? "h-1 animate-pulse" : "h-1"}
          />
        </div>
      ) : null}
    </div>
  );
}
