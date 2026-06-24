import { useState } from "react";
import { ChevronRight } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Spinner } from "@/components/ui/spinner";

/** Fold state for the Thinking block (#205): collapsed by default — even mid-stream —
 *  so a turn stays compact. A manual toggle (`userOpen`) wins for the rest of the turn. */
export function resolveThinkingOpen(userOpen: boolean | null): boolean {
  return userOpen ?? false;
}

/** Collapsible reasoning/thinking stream above the assistant answer (#181). */
export function ThinkingBlock({
  reasoning,
  streaming,
  hasAnswer,
}: {
  reasoning: string;
  streaming: boolean;
  hasAnswer: boolean;
}) {
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const open = resolveThinkingOpen(userOpen);

  if (!reasoning) return null;

  return (
    <div className="w-full font-mono text-[11px]">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setUserOpen(!open)}
        className="flex w-full items-center gap-1.5 rounded-md border border-border/60 bg-muted/40 px-2.5 py-1.5 text-left text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            open && "rotate-90",
          )}
        />
        {streaming && !hasAnswer && (
          <Spinner className="shrink-0 text-muted-foreground" />
        )}
        <span className="font-medium text-foreground/90">Thinking</span>
        {!open && (
          <span className="truncate text-muted-foreground/70">
            {reasoning.slice(0, 120)}
            {reasoning.length > 120 ? "…" : ""}
          </span>
        )}
      </button>
      {open && (
        <div
          data-selectable
          className="mt-1.5 max-h-48 overflow-y-auto whitespace-pre-wrap rounded-md border border-border/40 bg-muted/20 px-3 py-2 text-[11px] leading-relaxed text-muted-foreground"
        >
          {reasoning}
        </div>
      )}
    </div>
  );
}
