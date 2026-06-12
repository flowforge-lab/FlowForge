import { useState } from "react";
import {
  Check,
  ChevronRight,
  Loader2,
  PanelRight,
  ShieldAlert,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { ToolStep } from "@/store/chat";
import { useSplitStore } from "@/store/split";

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
  if (status === "awaiting-approval") {
    return <ShieldAlert className="size-3.5 text-amber-500" />;
  }
  if (status === "error") {
    return <X className="size-3.5 text-destructive" />;
  }
  return <Check className="size-3.5 text-emerald-500" />;
}

function SafetyBadge({ safety }: { safety: string }) {
  const dangerous = safety === "dangerous";
  return (
    <span
      className={cn(
        "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide",
        dangerous
          ? "bg-destructive/15 text-destructive"
          : "bg-amber-500/15 text-amber-700 dark:text-amber-400",
      )}
    >
      {safety}
    </span>
  );
}

export function ToolStepBlock({
  step,
  onRespond,
}: {
  step: ToolStep;
  onRespond: (callId: string, approved: boolean) => void;
}) {
  const awaiting = step.status === "awaiting-approval";
  const [userToggled, setUserToggled] = useState(false);
  // Force-open whenever the user must act (covers both the mock's same-tick
  // call+approval and the real backend, where the step mounts as "running" and
  // flips to awaiting later). Otherwise honor the user's manual toggle.
  const open = awaiting || userToggled;
  const openInSplit = useSplitStore((s) => s.openInSplit);
  const args = formatArgs(step.args);

  return (
    <div
      className={cn(
        "rounded-md border bg-muted/40 font-mono text-xs",
        awaiting && "border-amber-500/40 bg-amber-500/5",
      )}
    >
      <button
        type="button"
        onClick={() => setUserToggled((v) => !v)}
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
        {awaiting && step.safety && <SafetyBadge safety={step.safety} />}
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
          {awaiting && (
            <div className="flex items-center gap-2 pt-1">
              <Button
                size="sm"
                className="h-7 px-3 text-xs"
                onClick={() => onRespond(step.callId, true)}
              >
                Approve
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-7 px-3 text-xs"
                onClick={() => onRespond(step.callId, false)}
              >
                Deny
              </Button>
              <span className="text-[11px] text-muted-foreground/70">
                {step.safety === "dangerous"
                  ? "Destructive — review carefully."
                  : "This tool will modify your workspace."}
              </span>
            </div>
          )}
          {step.result !== undefined && (
            <div>
              <div className="mb-0.5 flex items-center justify-between">
                <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
                  output
                </span>
                <button
                  type="button"
                  onClick={() =>
                    openInSplit({
                      kind: "text",
                      text: step.result ?? "",
                      title: step.tool,
                    })
                  }
                  title="Open in split"
                  className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground"
                >
                  <PanelRight className="size-3" />
                  Split
                </button>
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
