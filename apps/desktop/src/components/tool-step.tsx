import { useState } from "react";
import { Check, ChevronRight, Loader2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";

function formatArgs(args: unknown): string {
  try {
    return JSON.stringify(args, null, 2);
  } catch {
    return String(args);
  }
}

function StatusIcon({ status }: { status: ToolStep["status"] }) {
  if (status === "running") {
    return <Loader2 className="size-3.5 animate-spin text-muted-foreground" />;
  }
  if (status === "error") {
    return <X className="size-3.5 text-destructive" />;
  }
  return <Check className="size-3.5 text-emerald-500" />;
}

export function ToolStepBlock({ step }: { step: ToolStep }) {
  const [open, setOpen] = useState(false);
  const args = formatArgs(step.args);

  return (
    <div className="rounded-md border bg-muted/40 font-mono text-xs">
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
        <StatusIcon status={step.status} />
        <span className="font-medium text-foreground">{step.tool}</span>
        {!open && args !== "{}" && (
          <span className="truncate text-muted-foreground/70">{args}</span>
        )}
      </button>
      {open && (
        <div className="space-y-2 border-t px-2.5 py-2">
          <div>
            <div className="mb-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/60">
              args
            </div>
            <pre className="overflow-x-auto whitespace-pre-wrap break-words text-foreground/90">
              {args}
            </pre>
          </div>
          {step.result !== undefined && (
            <div>
              <div className="mb-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/60">
                output
              </div>
              <pre
                className={cn(
                  "max-h-64 overflow-auto whitespace-pre-wrap break-words",
                  step.status === "error"
                    ? "text-destructive"
                    : "text-foreground/90",
                )}
              >
                {step.result}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
