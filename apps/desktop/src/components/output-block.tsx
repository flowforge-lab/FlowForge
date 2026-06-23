import { useState } from "react";
import { Check, ChevronRight, Copy, PanelRight } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useSplitStore } from "@/store/split";

/**
 * Tool output longer than this many characters starts folded (#331). Mirrors the
 * backend `TOOL_OUTPUT_FOLD_THRESHOLD` (`ff-session/src/lib.rs`) so "long enough to
 * fold" means the same in a live transcript, a reloaded transcript, and an
 * exported `.md`. Folding never truncates — the expanded view shows everything.
 */
export const OUTPUT_FOLD_THRESHOLD = 600;

/**
 * A neutral, collapsible block for a tool result (#331). Shared by the live
 * `ToolStepBlock` and the reloaded-transcript path in `chat-view`, so tool output
 * looks and folds identically before and after a session switch. Short output
 * renders inline; long output (`> OUTPUT_FOLD_THRESHOLD`) starts collapsed and
 * expands into a height-capped scrollable box (nothing is ever truncated).
 * `error` reserves the red treatment for genuine failures; normal output is neutral.
 */
export function OutputBlock({
  output,
  title,
  error = false,
  label = "output",
}: {
  output: string;
  title: string;
  error?: boolean;
  /** Header label; defaults to "output". */
  label?: string;
}) {
  const long = output.length > OUTPUT_FOLD_THRESHOLD;
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  // Short output is always shown; long output folds by default until toggled.
  const open = long ? (userOpen ?? false) : true;

  const openInSplit = useSplitStore((s) => s.openInSplit);
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(output);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard can be unavailable (permissions / insecure context); fail quiet.
    }
  }

  return (
    <div>
      <div className="mb-0.5 flex items-center justify-between gap-2">
        {long ? (
          <button
            type="button"
            aria-expanded={open}
            onClick={() => setUserOpen(!open)}
            className="flex min-w-0 items-center gap-1 text-[10px] uppercase tracking-wide text-muted-foreground/60 transition-colors hover:text-foreground"
          >
            <ChevronRight
              className={cn(
                "size-3 shrink-0 transition-transform",
                open && "rotate-90",
              )}
            />
            {label}
          </button>
        ) : (
          <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
            {label}
          </span>
        )}
        <div className="flex shrink-0 items-center gap-1">
          <button
            type="button"
            onClick={() => void copy()}
            title={copied ? "Copied" : "Copy output"}
            className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground"
          >
            {copied ? (
              <Check className="size-3 text-emerald-500" />
            ) : (
              <Copy className="size-3" />
            )}
            {copied ? "Copied" : "Copy"}
          </button>
          <button
            type="button"
            onClick={() => openInSplit({ kind: "text", text: output, title })}
            title="Open in split"
            className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground"
          >
            <PanelRight className="size-3" />
            Split
          </button>
        </div>
      </div>
      {open ? (
        <pre
          data-selectable
          className={cn(
            "max-h-64 overflow-auto whitespace-pre-wrap break-words",
            error ? "text-destructive" : "text-foreground/90",
          )}
        >
          {output}
        </pre>
      ) : (
        <button
          type="button"
          onClick={() => setUserOpen(true)}
          className="block w-full truncate text-left text-muted-foreground/70 transition-colors hover:text-foreground"
        >
          {output.slice(0, 120)}…
        </button>
      )}
    </div>
  );
}
