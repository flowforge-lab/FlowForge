import { AlertTriangle } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import {
  formatContextWindow,
  servedWindowSourceLabel,
  type ServedWindow,
} from "@/lib/served-window";

// Served context-window readout for a model (#602). Shows the window the provider
// is actually serving ("serving 128k · trained 256k") plus the source that produced
// it, so a mis-set server default is legible rather than a silent 32k under-fill.
// Pure presentational over a `ServedWindow` — the data is sourced per session in the
// model-chip; see lib/served-window.ts for the (proposed) contract.
export function ModelWindowInfo({ info }: { info: ServedWindow }) {
  const { window, trained, source } = info;
  const isDefault = source === "default";
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-center gap-1 text-xs text-foreground">
        <span className="font-medium tabular-nums">
          serving {formatContextWindow(window)}
        </span>
        {trained != null ? (
          <span className="text-muted-foreground tabular-nums">
            · trained {formatContextWindow(trained)}
          </span>
        ) : null}
      </div>
      <div
        className={cn(
          "flex items-center gap-1 text-[11px]",
          isDefault ? "text-amber-500" : "text-muted-foreground",
        )}
      >
        {isDefault ? <AlertTriangle className="size-3 shrink-0" /> : null}
        <span>{servedWindowSourceLabel(source)}</span>
      </div>
    </div>
  );
}
