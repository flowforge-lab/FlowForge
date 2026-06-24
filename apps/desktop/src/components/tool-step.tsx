import { useState } from "react";
import {
  Check,
  ChevronRight,
  CornerDownLeft,
  MessageCircleQuestion,
  ShieldAlert,
  X,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { formatArgs } from "@/lib/tool-args";
import { parseMcpToolName } from "@/lib/mcp";
import { describeStep } from "@/lib/step-describe";
import { formatDuration } from "@/lib/steps";
import { parseTodo } from "@/lib/todo";
import { TodoList } from "@/components/todo-list";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import type { ToolStep } from "@/store/chat";
import { OutputBlock } from "@/components/output-block";

function StatusIcon({ status }: { status: ToolStep["status"] }) {
  if (status === "running") {
    return <Spinner className="text-muted-foreground" />;
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
  return (
    <Badge tone={safety === "dangerous" ? "destructive" : "amber"}>
      {safety}
    </Badge>
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

/** Trust badge for a step approved via "Allow this session" / "Always allow"
 *  (#229). Sits next to the tool name on a resolved step. */
function TrustBadge({ kind }: { kind: "session" | "always" }) {
  return <Badge tone={kind === "always" ? "emerald" : "amber"}>{kind}</Badge>;
}

export function ToolStepBlock({
  step,
  onRespond,
  onApproveSession,
  onApproveAlways,
  onAnswer,
}: {
  step: ToolStep;
  onRespond: (callId: string, approved: boolean) => void;
  /** "Allow this session" — approves `tool` for the session, then this call.
   *  Required (like `onRespond`) so the approval buttons never silently no-op. */
  onApproveSession: (callId: string, tool: string) => void;
  /** "Always allow" — persists `tool`, then approves this call. */
  onApproveAlways: (callId: string, tool: string) => void;
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
  // Per-step duration (#413): shown once a step has settled (both timestamps
  // present); omitted while running or when timing is absent (e.g. reloaded rows
  // with no createdAt spread). Reuses the shared formatDuration — no new logic.
  const stepDurationMs =
    step.startedAt != null && step.finishedAt != null
      ? Math.max(0, step.finishedAt - step.startedAt)
      : null;

  return (
    <div
      className={cn(
        "rounded-md border bg-muted/40 font-mono text-[11px]",
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
        {!awaiting && step.alwaysApproved && <TrustBadge kind="always" />}
        {!awaiting && !step.alwaysApproved && step.sessionApproved && (
          <TrustBadge kind="session" />
        )}
        {asking && <Badge tone="sky">needs answer</Badge>}
        {stepDurationMs !== null && (
          <span className="ml-auto shrink-0 font-normal text-muted-foreground/60">
            {formatDuration(stepDurationMs)}
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
            <div className="space-y-1.5 pt-1">
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  size="sm"
                  className="h-7 px-3 text-xs"
                  onClick={() => onRespond(step.callId, true)}
                >
                  Allow once
                </Button>
                {/* Dangerous tools are never covered by the session/always
                    allowlist (#232: `allowlist_covers` excludes Dangerous), so
                    only offer the persistent tiers for non-dangerous calls —
                    otherwise the grant would be written but the backend would
                    keep prompting. Dangerous steps get Allow once / Deny only. */}
                {step.safety !== "dangerous" && (
                  <>
                    <Button
                      size="sm"
                      variant="secondary"
                      className="h-7 px-3 text-xs"
                      onClick={() => onApproveSession(step.callId, step.tool)}
                    >
                      Allow this session
                    </Button>
                    <Button
                      size="sm"
                      variant="secondary"
                      className="h-7 px-3 text-xs"
                      onClick={() => onApproveAlways(step.callId, step.tool)}
                    >
                      Always allow
                    </Button>
                  </>
                )}
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 px-3 text-xs"
                  onClick={() => onRespond(step.callId, false)}
                >
                  Deny
                </Button>
              </div>
              <span className="text-[11px] text-muted-foreground/70">
                {step.safety === "dangerous"
                  ? "Destructive — review carefully."
                  : "This tool will modify your workspace."}
              </span>
            </div>
          )}
          {step.result !== undefined && (
            <OutputBlock
              output={step.result}
              title={step.tool}
              error={step.status === "error"}
            />
          )}
        </div>
      )}
    </div>
  );
}
