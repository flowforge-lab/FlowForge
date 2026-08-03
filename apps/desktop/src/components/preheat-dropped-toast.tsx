import { useMemo } from "react";
import { Zap, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Toast, ToastViewport } from "@/components/ui/toast";
import { usePreheatNoticeStore } from "@/store/preheat-notice";

/**
 * Toasts the `phenotype:preheat-dropped` notice (#1179): a just-activated phenotype
 * asked to preheat tools that could not all be admitted to the resident block.
 *
 * Mirrors <PhenoMcpToast>. Two distinct causes share one notice because they share a
 * fix (edit the phenotype's `preheat` list), but they read differently, so the body
 * names them separately: `unknown` is almost always a typo, while `overBudget` means
 * the list is simply too long and the tail lost.
 *
 * Deliberately does NOT auto-dismiss (#573 2c): the dropped tools stay dropped for
 * the whole session, so a toast that vanished on a timer would be the only signal a
 * misconfigured phenotype ever gives, and it would be easy to miss.
 */
export function PreheatDroppedToast() {
  const notice = usePreheatNoticeStore((s) => s.notice);
  const dismiss = usePreheatNoticeStore((s) => s.dismiss);

  const lines = useMemo(() => {
    if (!notice) return [];
    const out: string[] = [];
    if (notice.unknown.length > 0) {
      out.push(
        `No such tool: ${notice.unknown.join(", ")} — check for a typo, or the tool may not be deferrable.`,
      );
    }
    if (notice.overBudget.length > 0) {
      out.push(
        `Over the preheat budget: ${notice.overBudget.join(", ")} — the list is too long, so these were left behind.`,
      );
    }
    return out;
  }, [notice]);

  if (!notice) return null;

  return (
    <ToastViewport>
      <Toast>
        <div className="flex items-start gap-3">
          <Zap className="mt-0.5 size-4 shrink-0 text-amber-500" />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">
              Some preheat tools were skipped
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              Phenotype <span className="font-mono">{notice.phenotype}</span>{" "}
              activated, but not everything in its preheat list made it in.
              These tools still work — the assistant reaches them via{" "}
              <span className="font-mono">tool_search</span>.
            </p>
            {lines.map((line) => (
              <p key={line} className="mt-1 text-xs text-muted-foreground">
                {line}
              </p>
            ))}
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="size-6 shrink-0"
            onClick={dismiss}
            aria-label="Dismiss"
          >
            <X className="size-3.5" />
          </Button>
        </div>
      </Toast>
    </ToastViewport>
  );
}
