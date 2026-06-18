import { useState } from "react";
import {
  Check,
  ChevronRight,
  CornerDownLeft,
  Loader2,
  MessageCircleQuestion,
  PanelRight,
  ShieldAlert,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { formatArgs } from "@/lib/tool-args";
import { parseMcpToolName } from "@/lib/mcp";
import { describeStep } from "@/lib/step-describe";
import { parseTodo } from "@/lib/todo";
import { TodoList } from "@/components/todo-list";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import type { ToolStep } from "@/store/chat";
import { useSplitStore } from "@/store/split";

function StatusIcon({ status }: { status: ToolStep["status"] }) {
  if (status === "running") {
    return <Loader2 className="size-3.5 animate-spin text-muted-foreground" />;
  }
  if (status === "awaiting-approval") {
    return <ShieldAlert className="size-3.5 text-amber-500" />;
  }
  if (status === "awaiting-answer") {
    return <MessageCircleQuestion className="size-3.5 text-sky-500" />;
  }
  if (status === "error") {
    return <X className="size-3.5 text-destructive" />;
  }
  return <Check className="size-3.5 text-emerald-500" />;
}

// Inline prompt for an `ask_user` step (#44): shows the agent's question and a
// reply box. Submit (Enter, or the button) sends the answer back as the tool
// result; Shift+Enter inserts a newline. Dismissing is the turn's Stop button —
// the backend resolves a dropped ask without a hang, so there's no deny here.
function AskPrompt({
  question,
  onSubmit,
}: {
  question: string;
  onSubmit: (answer: string) => void;
}) {
  const [answer, setAnswer] = useState("");
  const canSubmit = answer.trim().length > 0;

  const submit = () => {
    if (!canSubmit) return;
    onSubmit(answer.trim());
  };

  return (
    <div className="space-y-2 border-t px-2.5 py-2">
      <p className="whitespace-pre-wrap font-sans text-[13px] leading-relaxed text-foreground">
        {question}
      </p>
      <Textarea
        autoFocus
        rows={2}
        value={answer}
        placeholder="Type your answer…"
        className="min-h-16 font-sans text-[13px]"
        onChange={(e) => setAnswer(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          className="h-7 px-3 text-xs"
          disabled={!canSubmit}
          onClick={submit}
        >
          Answer
        </Button>
        <span className="flex items-center gap-1 text-[11px] text-muted-foreground/70">
          <CornerDownLeft className="size-3" />
          to send · Shift+Enter for a new line
        </span>
      </div>
    </div>
  );
}

function SafetyBadge({ safety }: { safety: NonNullable<ToolStep["safety"]> }) {
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

/** MCP tools render as a server badge + bare tool name (#91). */
function ToolLabel({ tool }: { tool: string }) {
  const mcp = parseMcpToolName(tool);
  if (!mcp) {
    return <span className="font-medium text-foreground">{tool}</span>;
  }
  return (
    <span className="flex items-center gap-1.5">
      <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
        {mcp.server}
      </span>
      <span className="font-medium text-foreground">{mcp.tool}</span>
    </span>
  );
}

export function ToolStepBlock({
  step,
  onRespond,
  onAnswer,
}: {
  step: ToolStep;
  onRespond: (callId: string, approved: boolean) => void;
  onAnswer?: (callId: string, answer: string) => void;
}) {
  const awaiting = step.status === "awaiting-approval";
  const asking = step.status === "awaiting-answer";
  const [userToggled, setUserToggled] = useState(false);
  // The `todo` tool renders its checklist from the call args (Issue #42). null =
  // not a todo step → fall back to the generic args/result render.
  const todoItems = step.tool === "todo" ? parseTodo(step.args) : null;
  // Force-open whenever the user must act (an approval gate or an `ask_user`
  // prompt — covers both the mock's same-tick call+request and the real backend,
  // where the step mounts as "running" and flips later). A todo plan is meant to
  // be seen, so it defaults to open; other tools default to collapsed. Otherwise
  // honor the manual toggle.
  const open =
    awaiting || asking || (todoItems !== null ? !userToggled : userToggled);
  const openInSplit = useSplitStore((s) => s.openInSplit);
  const args = formatArgs(step.args);
  const mcp = parseMcpToolName(step.tool);
  const description = describeStep(step);
  // The question travels on the step (set by applyAskRequest); fall back to the
  // call args so the prompt still renders if the event lost the text.
  const question =
    step.question ??
    (typeof (step.args as { question?: unknown } | null)?.question === "string"
      ? (step.args as { question: string }).question
      : "");

  return (
    <div
      className={cn(
        "rounded-md border bg-muted/40 font-mono text-xs",
        awaiting && "border-amber-500/40 bg-amber-500/5",
        asking && "border-sky-500/40 bg-sky-500/5",
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
        <span className="min-w-0 truncate font-medium text-foreground">
          {mcp ? <ToolLabel tool={step.tool} /> : description}
        </span>
        {awaiting && step.safety && <SafetyBadge safety={step.safety} />}
        {asking && (
          <span className="rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-sky-700 dark:text-sky-400">
            needs answer
          </span>
        )}
      </button>
      {open && asking && (
        <AskPrompt
          question={question}
          onSubmit={(answer) => onAnswer?.(step.callId, answer)}
        />
      )}
      {open && !asking && todoItems !== null && (
        <div className="border-t px-2.5 py-2">
          <TodoList items={todoItems} />
        </div>
      )}
      {open && !asking && todoItems === null && (
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
