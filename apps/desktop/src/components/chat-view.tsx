import { memo, useEffect, useRef } from "react";
import { cn } from "@/lib/utils";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";
import { ToolStepBlock } from "@/components/tool-step";
import { StepGroup } from "@/components/step-group";
import { ThinkingBlock } from "@/components/thinking-block";
import { Markdown } from "@/components/markdown";
import { MessageActions } from "@/components/message-actions";
import { ThinkingIndicator } from "@/components/thinking-indicator";
import type { Message } from "@/bindings";

const NO_STEPS: ToolStep[] = [];

function MessageRowImpl({
  message,
  streaming,
  toolSteps,
  turnStartMs,
  reasoning,
  respondApproval,
  respondAsk,
}: {
  message: Message;
  streaming: boolean;
  toolSteps: ToolStep[];
  turnStartMs?: number | null;
  reasoning: string;
  respondApproval: (
    sessionId: string,
    messageId: string,
    callId: string,
    approved: boolean,
  ) => Promise<void>;
  respondAsk: (
    sessionId: string,
    messageId: string,
    callId: string,
    answer: string,
  ) => Promise<void>;
}) {
  const onRespond = (callId: string, approved: boolean) =>
    void respondApproval(message.sessionId, message.id, callId, approved);
  const onAnswer = (callId: string, answer: string) =>
    void respondAsk(message.sessionId, message.id, callId, answer);
  if (message.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="group relative max-w-[80%]">
          <div
            data-selectable
            className="whitespace-pre-wrap rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-[13px] leading-relaxed text-primary-foreground shadow-sm"
          >
            {message.content}
          </div>
          <MessageActions message={message} side="left" />
        </div>
      </div>
    );
  }

  if (message.role === "system" || message.role === "tool") {
    return (
      <div
        data-selectable
        className="whitespace-pre-wrap rounded-md border border-destructive/30 bg-destructive/10 px-3 py-1.5 font-mono text-xs leading-relaxed text-muted-foreground"
      >
        {message.content}
      </div>
    );
  }

  return (
    <div className="flex flex-col items-start gap-1.5">
      {toolSteps.length > 0 ? (
        <div className="flex w-full max-w-[80%] flex-col gap-1.5">
          {/* Single settled step stays bare; streaming (any count), 2+ steps, or any
              reasoning to fold in (#205) use StepGroup so the live timer, peek window
              (#180), and grouped Thinking block apply. */}
          {toolSteps.length === 1 && !streaming && !reasoning ? (
            <ToolStepBlock
              step={toolSteps[0]}
              onRespond={onRespond}
              onAnswer={onAnswer}
            />
          ) : (
            <StepGroup
              steps={toolSteps}
              streaming={streaming}
              turnStartMs={turnStartMs}
              reasoning={reasoning}
              hasAnswer={message.content.length > 0}
              onRespond={onRespond}
              onAnswer={onAnswer}
            />
          )}
        </div>
      ) : (
        // No tool steps this turn: the Thinking block stands alone, but folded by
        // default (#205) so it stays compact.
        reasoning && (
          <div className="w-full max-w-[80%]">
            <ThinkingBlock
              reasoning={reasoning}
              streaming={streaming}
              hasAnswer={message.content.length > 0}
            />
          </div>
        )
      )}
      {message.content && (
        <div className="group relative max-w-[80%]">
          <div
            data-selectable
            className={cn(
              "px-0.5 py-1 text-[13px] leading-relaxed text-foreground",
              streaming && "ff-streaming-caret",
            )}
          >
            <Markdown content={message.content} streaming={streaming} />
          </div>
          <MessageActions message={message} side="right" />
        </div>
      )}
    </div>
  );
}

// Memoized so a per-token state commit on the streaming message does not re-render
// every other row. Props are referentially stable (the store actions are stable;
// `NO_STEPS` avoids a fresh `[]` per render), so only the changed row re-renders.
const MessageRow = memo(MessageRowImpl);

// `sessionId` scopes the view to one session so split panes (#148) each render
// an independent transcript. Defaults to the active session for the single-pane
// layout.
export function ChatView({ sessionId }: { sessionId?: string } = {}) {
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const targetSessionId = sessionId ?? activeSessionId ?? undefined;
  const messages = useChatStore((s) =>
    targetSessionId ? s.messagesBySession[targetSessionId] : undefined,
  );
  const streamingId = useChatStore((s) =>
    targetSessionId ? s.streamingBySession[targetSessionId] : undefined,
  );
  // Pending = the turn is in flight but nothing has streamed yet; show a
  // "thinking" indicator until the first token/tool-call materializes a row.
  const pending = useChatStore((s) =>
    targetSessionId
      ? Boolean(s.turnStartBySession[targetSessionId]) &&
        !s.streamingBySession[targetSessionId]
      : false,
  );
  const toolStepsByMessage = useChatStore((s) => s.toolStepsByMessage);
  const turnStartByMessage = useChatStore((s) => s.turnStartByMessage);
  const turnStartBySession = useChatStore((s) => s.turnStartBySession);
  const reasoningByMessage = useChatStore((s) => s.reasoningByMessage);
  const respondApproval = useChatStore((s) => s.respondApproval);
  const respondAsk = useChatStore((s) => s.respondAsk);

  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  // Stay pinned to the bottom while streaming, but respect manual scroll-up.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedToBottom.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, toolStepsByMessage, reasoningByMessage, pending]);

  useEffect(() => {
    pinnedToBottom.current = true;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [targetSessionId]);

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    pinnedToBottom.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  }

  if (!messages || messages.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-1.5">
        <p className="text-base font-medium text-foreground/80">
          What are you working on?
        </p>
        <p className="text-[13px] text-muted-foreground/70">
          Enter to send · Shift+Enter for a new line · Esc to stop ·{" "}
          <kbd className="font-mono">?</kbd> for shortcuts
        </p>
      </div>
    );
  }

  return (
    <div
      ref={scrollRef}
      onScroll={handleScroll}
      className="flex-1 overflow-y-auto"
    >
      <div className="mx-auto flex max-w-3xl flex-col gap-3 px-4 py-4">
        {messages.map((m) => (
          <MessageRow
            key={m.id}
            message={m}
            streaming={m.id === streamingId}
            toolSteps={toolStepsByMessage[m.id] ?? NO_STEPS}
            turnStartMs={
              turnStartByMessage[m.id] ?? turnStartBySession[m.sessionId]
            }
            reasoning={reasoningByMessage[m.id] ?? ""}
            respondApproval={respondApproval}
            respondAsk={respondAsk}
          />
        ))}
        {pending && <ThinkingIndicator />}
      </div>
    </div>
  );
}
