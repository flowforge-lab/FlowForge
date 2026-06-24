import { useState } from "react";
import { ChevronRight, FileText } from "@/components/ui/icon";
import { cn } from "@/lib/utils";

/** First non-empty line of the prose, for the collapsed one-line header. */
function firstLine(text: string): string {
  return text.split("\n").find((l) => l.trim()) ?? text.trim();
}

/**
 * A folded row for the model's intermediate prose between tool calls (#415) — the
 * "Now let me check …" narration the backend persists on each per-iteration assistant
 * message. Mirrors ToolStepBlock's collapsed aesthetic, distinguished by a doc icon;
 * the full prose expands on click. Pure presentation — the text is the raw content.
 */
export function ProseStepBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);

  return (
    <div className="rounded-md border bg-muted/40 font-mono text-[11px]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            open && "rotate-90",
          )}
        />
        <FileText className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate text-foreground/80">
          {firstLine(text)}
        </span>
      </button>
      {open && (
        <p className="whitespace-pre-wrap border-t px-2.5 py-2 font-sans text-[12px] leading-relaxed text-foreground/90">
          {text}
        </p>
      )}
    </div>
  );
}
