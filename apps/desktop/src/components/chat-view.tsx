import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";
import { ToolStepBlock } from "@/components/tool-step";
import type { Message } from "@/bindings";

function MessageRow({
  message,
  streaming,
  toolSteps,
}: {
  message: Message;
  streaming: boolean;
  toolSteps: ToolStep[];
}) {
  if (message.role === "user") {
    return (
      <div className="flex justify-end">
        <div
          data-selectable
          className="max-w-[80%] whitespace-pre-wrap rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-[13px] leading-relaxed text-primary-foreground shadow-sm"
        >
          {message.content}
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
      {toolSteps.length > 0 && (
        <div className="flex w-full max-w-[80%] flex-col gap-1.5">
          {toolSteps.map((step) => (
            <ToolStepBlock key={step.callId} step={step} />
          ))}
        </div>
      )}
      {message.content && (
        <div
          data-selectable
          className={cn(
            "max-w-[80%] whitespace-pre-wrap px-0.5 py-1 text-[13px] leading-relaxed text-foreground",
            streaming && "ff-streaming-caret",
          )}
        >
          {message.content}
        </div>
      )}
    </div>
  );
}

export function ChatView() {
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const messages = useChatStore((s) =>
    s.activeSessionId ? s.messagesBySession[s.activeSessionId] : undefined,
  );
  const streamingId = useChatStore((s) =>
    s.activeSessionId ? s.streamingBySession[s.activeSessionId] : undefined,
  );
  const toolStepsByMessage = useChatStore((s) => s.toolStepsByMessage);

  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  // Stay pinned to the bottom while streaming, but respect manual scroll-up.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedToBottom.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, toolStepsByMessage]);

  useEffect(() => {
    pinnedToBottom.current = true;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [activeSessionId]);

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
          Enter to send · Shift+Enter for a new line · Esc to stop
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
            toolSteps={toolStepsByMessage[m.id] ?? []}
          />
        ))}
      </div>
    </div>
  );
}
