import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";
import { ToolStepBlock } from "@/components/tool-step";
import { OutputBlock } from "@/components/output-block";
import { StepGroup } from "@/components/step-group";
import { ThinkingBlock } from "@/components/thinking-block";
import { Markdown } from "@/components/markdown";
import { MessageActions } from "@/components/message-actions";
import { MessageAttachments } from "@/components/message-attachments";
import { ThinkingIndicator } from "@/components/thinking-indicator";
import { foldTurns } from "@/lib/turn-groups";
import type { TurnItem } from "@/lib/turn-groups";
import { useExperimentalStore } from "@/store/experimental";
import { useModelConfigStore, activeConnection } from "@/store/model-config";
import { downloadStepTimeline } from "@/lib/export-step-timeline";
import type { Message } from "@/bindings";

const NO_STEPS: ToolStep[] = [];
const NO_ITEMS: TurnItem[] = [];

function MessageRowImpl({
  message,
  streaming,
  toolSteps,
  items,
  turnStartMs,
  reasoning,
  exportEnabled,
  exportModel,
  exportTiming,
  respondApproval,
  approveSession,
  approveAlways,
  respondAsk,
}: {
  message: Message;
  streaming: boolean;
  toolSteps: ToolStep[];
  items: TurnItem[];
  turnStartMs?: number | null;
  reasoning: string;
  /** Dev step-timeline export gated by the experimental flag (#417). */
  exportEnabled: boolean;
  exportModel: string | null;
  exportTiming: "exact" | "approx-created-at";
  respondApproval: (
    sessionId: string,
    messageId: string,
    callId: string,
    approved: boolean,
  ) => Promise<void>;
  approveSession: (
    sessionId: string,
    messageId: string,
    callId: string,
    tool: string,
  ) => Promise<void>;
  approveAlways: (
    sessionId: string,
    messageId: string,
    callId: string,
    tool: string,
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
  const onApproveSession = (callId: string, tool: string) =>
    void approveSession(message.sessionId, message.id, callId, tool);
  const onApproveAlways = (callId: string, tool: string) =>
    void approveAlways(message.sessionId, message.id, callId, tool);
  const onAnswer = (callId: string, answer: string) =>
    void respondAsk(message.sessionId, message.id, callId, answer);
  if (message.role === "user") {
    const hasAttachments =
      message.attachments && message.attachments.length > 0;
    return (
      <div className="flex flex-col items-end gap-1.5">
        {hasAttachments && (
          <div className="max-w-[80%]">
            <MessageAttachments attachments={message.attachments!} />
          </div>
        )}
        {/* Skip the bubble entirely for an image-only message (no text). */}
        {message.content && (
          <div className="group relative max-w-[80%]">
            <div
              data-selectable
              className="whitespace-pre-wrap rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-[13px] leading-relaxed text-primary-foreground shadow-sm"
            >
              {message.content}
            </div>
            <MessageActions message={message} side="left" />
          </div>
        )}
      </div>
    );
  }

  // Persisted tool/system rows on session reload (#331). Render through the same
  // collapsible OutputBlock as a live step so reloaded output looks and folds
  // identically — neutral, not the old red dump. A persisted Message carries no
  // status, so there's no error signal here; red is reserved for the live step
  // path (where `status` exists).
  if (message.role === "system" || message.role === "tool") {
    return (
      <div className="w-full max-w-[80%] rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-[11px] leading-relaxed">
        <OutputBlock output={message.content} title="tool output" />
      </div>
    );
  }

  return (
    <div className="flex flex-col items-start gap-1.5">
      {toolSteps.length > 0 ? (
        <div className="flex w-full max-w-[80%] flex-col gap-1.5">
          {/* Single settled step stays bare; streaming (any count), 2+ steps, any
              reasoning to fold in (#205), or any intermediate prose to interleave
              (#415) use StepGroup so the live timer, peek window (#180), grouped
              Thinking block, and folded prose rows apply. */}
          {toolSteps.length === 1 &&
          !streaming &&
          !reasoning &&
          !items.some((it) => it.kind === "prose") ? (
            <ToolStepBlock
              step={toolSteps[0]}
              onRespond={onRespond}
              onApproveSession={onApproveSession}
              onApproveAlways={onApproveAlways}
              onAnswer={onAnswer}
            />
          ) : (
            <StepGroup
              steps={toolSteps}
              items={items}
              streaming={streaming}
              turnStartMs={turnStartMs}
              reasoning={reasoning}
              hasAnswer={message.content.length > 0}
              answer={message.content}
              onExportTimeline={
                exportEnabled
                  ? (format) =>
                      void downloadStepTimeline(
                        toolSteps,
                        {
                          sessionId: message.sessionId,
                          model: exportModel,
                          timing: exportTiming,
                          capturedAt: Date.now(),
                        },
                        format,
                      )
                  : undefined
              }
              onRespond={onRespond}
              onApproveSession={onApproveSession}
              onApproveAlways={onApproveAlways}
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
              "px-0.5 py-1 text-sm leading-relaxed text-foreground",
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
  // Dev step-timeline export (#417): the affordance shows only with the flag on; the
  // active model id is stamped into the dump's meta.
  const exportEnabled = useExperimentalStore((s) => s.flags.stepTimelineExport);
  const exportModel = useModelConfigStore(
    (s) => activeConnection(s.registry)?.model ?? null,
  );
  const respondApproval = useChatStore((s) => s.respondApproval);
  const approveSession = useChatStore((s) => s.approveSession);
  const approveAlways = useChatStore((s) => s.approveAlways);
  const respondAsk = useChatStore((s) => s.respondAsk);

  // Fold the transcript into per-turn render groups (#413/#415). Steps are resolved
  // per assistant message: live `toolStepsByMessage` while streaming (aggregated
  // across every iteration of a multi-step turn), or reconstructed from the persisted
  // tool/system messages on reload. Intermediate prose interleaves as folded rows.
  const groups = useMemo(
    () => foldTurns(messages ?? [], toolStepsByMessage),
    [messages, toolStepsByMessage],
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);
  // Render-state mirror of `pinnedToBottom` so the floating "Jump to latest" button
  // (#206) shows only while scrolled up. Toggles at the same 40px threshold.
  const [atBottom, setAtBottom] = useState(true);

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

  // Reset the scroll affordance when the pane switches sessions. setState during
  // render is React's recommended reset-on-prop-change pattern — no effect, no
  // flash of the button before a scroll event would otherwise clear it. (The
  // `pinnedToBottom` ref is re-armed by the session-switch effect above.)
  const [renderedSession, setRenderedSession] = useState(targetSessionId);
  if (renderedSession !== targetSessionId) {
    setRenderedSession(targetSessionId);
    setAtBottom(true);
  }

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const pinned = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    pinnedToBottom.current = pinned;
    setAtBottom(pinned);
  }

  // Smooth-scroll to the newest content and re-arm sticky autoscroll (#206).
  function jumpToLatest() {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    pinnedToBottom.current = true;
    setAtBottom(true);
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
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="min-h-0 flex-1 overflow-y-auto"
      >
        <div className="mx-auto flex max-w-3xl flex-col gap-3 px-4 py-4">
          {groups.map((g) => {
            const m = g.message;
            // foldTurns already resolved each turn's steps (live or reconstructed)
            // and interleaved prose items (#415).
            const toolSteps = g.kind === "assistant" ? g.steps : NO_STEPS;
            const items = g.kind === "assistant" ? g.items : NO_ITEMS;
            // Prefer live reasoning when present; fall back to the reasoning
            // persisted on the assistant message for a reloaded turn (#375).
            const reasoning =
              reasoningByMessage[m.id] ??
              (g.kind === "assistant" ? g.reasoning : "");
            // Live steps (this session) carry exact wall-clock timing; a reloaded
            // turn's steps are reconstructed from `createdAt`, so tag it approx (#417).
            const liveTiming = (toolStepsByMessage[m.id]?.length ?? 0) > 0;
            return (
              <MessageRow
                key={m.id}
                message={m}
                streaming={m.id === streamingId}
                toolSteps={toolSteps}
                items={items}
                turnStartMs={
                  turnStartByMessage[m.id] ?? turnStartBySession[m.sessionId]
                }
                reasoning={reasoning}
                exportEnabled={exportEnabled}
                exportModel={exportModel}
                exportTiming={liveTiming ? "exact" : "approx-created-at"}
                respondApproval={respondApproval}
                approveSession={approveSession}
                approveAlways={approveAlways}
                respondAsk={respondAsk}
              />
            );
          })}
          {pending && <ThinkingIndicator />}
        </div>
      </div>

      {/* Floating "Jump to latest" — only while scrolled up (#206). A primary dot
          flags new content arriving below during an active stream. */}
      {!atBottom && (
        <button
          type="button"
          onClick={jumpToLatest}
          aria-label="Jump to latest"
          title="Jump to latest"
          className="absolute bottom-3 left-1/2 z-10 flex size-8 -translate-x-1/2 items-center justify-center rounded-full border bg-background/95 text-muted-foreground shadow-md backdrop-blur transition-colors hover:text-foreground"
        >
          <ChevronDown className="size-4" />
          {streamingId && (
            <span className="absolute -right-0.5 -top-0.5 size-2 rounded-full bg-primary ring-2 ring-background" />
          )}
        </button>
      )}
    </div>
  );
}
