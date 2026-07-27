import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { ChevronDown } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";
import { useFindStore } from "@/store/find";
import { ToolStepBlock } from "@/components/tool-step";
import { OutputBlock } from "@/components/output-block";
import { StepGroup } from "@/components/step-group";
import { ThinkingBlock } from "@/components/thinking-block";
import { Markdown } from "@/components/markdown";
import {
  MessageActions,
  ResponseCopyButton,
} from "@/components/message-actions";
import { MessageAttachments } from "@/components/message-attachments";
import { MessageHeader } from "@/components/message-header";
import { MessageEditBox } from "@/components/message-edit-box";
import { ThinkingIndicator } from "@/components/thinking-indicator";
import { CancelledNotice } from "@/components/cancelled-notice";
import { ActiveProseBlock } from "@/components/active-prose-block";
import { isResumableStopNotice } from "@/store/capped-turn";
import { foldTurns, lastTurnStart, segmentTurn } from "@/lib/turn-groups";
import type { TurnItem } from "@/lib/turn-groups";
import { useExperimentalStore } from "@/store/experimental";
import { useModelConfigStore, activeConnection } from "@/store/model-config";
import { downloadStepTimeline } from "@/lib/export-step-timeline";
import type { Message } from "@/bindings";

const NO_STEPS: ToolStep[] = [];
const NO_ITEMS: TurnItem[] = [];
// Stable empty transcript so a not-yet-loaded session keeps a constant `msgs` ref.
const EMPTY_MESSAGES: Message[] = [];

// Narrow a message-id-keyed store record to just the entries belonging to one
// pane's messages (#1009). These maps (`toolStepsByMessage` / `turnStartByMessage`
// / `reasoningByMessage`) are shared across every session, so their top-level ref
// changes on *any* session's token. Subscribing to the whole map re-renders this
// `ChatView` on foreign panes' tokens; scoping to this session's ids first — then
// comparing shallowly — keeps foreign keys out of the compared value so only this
// session's own churn re-renders the pane.
function scopeByIds<T>(
  map: Record<string, T>,
  msgs: Message[] | undefined,
): Record<string, T> {
  const out: Record<string, T> = {};
  if (msgs) for (const m of msgs) if (m.id in map) out[m.id] = map[m.id];
  return out;
}

function MessageRowImpl({
  message,
  streaming,
  toolSteps,
  hasOwnLiveSteps,
  items,
  turnStartMs,
  reasoning,
  exportEnabled,
  exportModel,
  exportTiming,
  isEditing,
  isLastUserMessage,
  beginEdit,
  endEdit,
  respondApproval,
  approveSession,
  approveAlways,
  respondAsk,
}: {
  message: Message;
  streaming: boolean;
  toolSteps: ToolStep[];
  /** True when `message` (the turn's current iteration) already has its own
   *  tool call recorded — i.e. it's *guaranteed* not the final answer, even
   *  though it's still the one streaming (#864). Gates the answer slot's
   *  "On it" collapse: a genuine final answer (no tool call of its own) never
   *  collapses, matching the issue's "final answer is never eligible" rule. */
  hasOwnLiveSteps: boolean;
  items: TurnItem[];
  turnStartMs?: number | null;
  reasoning: string;
  /** Dev step-timeline export gated by the experimental flag (#417). */
  exportEnabled: boolean;
  exportModel: string | null;
  exportTiming: "exact" | "approx-created-at";
  /** This user message is open in the bubble-anchored edit box (#929 A). */
  isEditing: boolean;
  /** This is the tail user message — the only one that offers Retry (#929 C). */
  isLastUserMessage: boolean;
  /** Stable callbacks owned by ChatView; stable so `memo` still holds. */
  beginEdit: (messageId: string) => void;
  endEdit: () => void;
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
  // Save from the inline edit box routes through the existing truncate-and-rerun
  // path (#929 A) — same call the composer used to make.
  const editMessage = useChatStore((s) => s.editMessage);
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
      <div
        data-message-id={message.id}
        className="flex flex-col items-end gap-1.5"
      >
        <MessageHeader
          role="user"
          createdAt={message.createdAt}
          messageId={message.id}
        />
        {hasAttachments && (
          <div className="max-w-[80%]">
            <MessageAttachments attachments={message.attachments!} />
          </div>
        )}
        {/* Edit opens a contained box anchored to THIS bubble (#929 A) rather than
            routing the text into the shared composer — the old flow left the user
            unsure whether they were editing or composing. Attachments pass straight
            through: the backend replaces that column wholesale, so omitting them on
            save would silently destroy them. */}
        {isEditing ? (
          <div className="w-[80%]">
            <MessageEditBox
              initialText={message.content}
              onSave={(text) => {
                endEdit();
                void editMessage(
                  message.sessionId,
                  message.id,
                  text,
                  message.attachments ?? undefined,
                );
              }}
              onCancel={endEdit}
            />
          </div>
        ) : (
          /* Skip the bubble entirely for an image-only message (no text). */
          message.content && (
            <div className="group relative max-w-[80%]">
              <div
                data-selectable
                className="whitespace-pre-wrap rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-[15px] leading-relaxed text-primary-foreground shadow-sm"
              >
                {message.content}
              </div>
              <MessageActions
                message={message}
                side="left"
                isLastUserMessage={isLastUserMessage}
                onBeginEdit={() => beginEdit(message.id)}
              />
            </div>
          )
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
      <div
        data-message-id={message.id}
        className="w-full max-w-[80%] rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-[11px] leading-relaxed"
      >
        <OutputBlock
          output={message.content}
          title="tool output"
          // Persisted tool row: its body can fold at OUTPUT_FOLD_THRESHOLD.
          // `expandId` keyed by messageId so the find bar (#875) can force
          // this block open when a match lives inside its body — same bus as
          // the live step path uses, scoped here to the persisted row's id.
          expandId={`output:${message.id}`}
        />
      </div>
    );
  }

  // Split the turn on its intermediate prose (#619): prose renders top-level and
  // always-visible; each contiguous reasoning+steps run is one collapsible group.
  // The live timer / answer preview / export belong to the LAST steps group — it's
  // the one that precedes the final answer.
  const segments = segmentTurn(items);
  let firstStepsIdx = -1;
  let lastStepsIdx = -1;
  let lastProseIdx = -1;
  segments.forEach((seg, i) => {
    if (seg.kind === "steps") {
      if (firstStepsIdx === -1) firstStepsIdx = i;
      lastStepsIdx = i;
    } else {
      lastProseIdx = i;
    }
  });
  // The "active" prose is the prose segment the model is *currently* writing
  // — i.e. the LAST segment overall. When prose is followed by more steps
  // (a `prose → steps` order), the prose is already settled and the steps
  // below are what's live; collapsing the prose to "On it" there would land
  // the chip on a done segment (#864 review).
  const lastProseIsActive = lastProseIdx > lastStepsIdx;

  return (
    <div
      data-message-id={message.id}
      className="flex flex-col items-start gap-1.5"
    >
      <MessageHeader
        role="assistant"
        createdAt={message.createdAt}
        authorName={message.authorName}
      />
      {toolSteps.length > 0 ? (
        <div className="flex w-full flex-col gap-1.5">
          {/* A single settled step stays bare; streaming (any count), 2+ steps, or
              any reasoning to fold in (#205) use StepGroup so the live timer, peek
              window (#180), and inline Thinking rows (#574) apply. Intermediate prose
              (#619) forces the segmented path below so it renders top-level. */}
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
            segments.map((seg, i) =>
              seg.kind === "prose" ? (
                // Intermediate narration — a top-level, always-visible block
                // between the collapsed groups it sat between (#619). Muted vs.
                // the final answer. The currently-streaming one collapses to a
                // compact "On it" chip so the user isn't forced to read it
                // token by token (#864); earlier prose and settled turns stay
                // full-width. Gated on the prose being the *last* segment so a
                // `prose → steps` order (prose already settled, model on to
                // tools) doesn't land a chip on a done segment.
                <ActiveProseBlock
                  key={`prose:${seg.key}`}
                  text={seg.text}
                  streaming={
                    i === lastProseIdx && lastProseIsActive && streaming
                  }
                />
              ) : (
                <StepGroup
                  key={`steps:${seg.key}`}
                  steps={seg.steps}
                  items={seg.items}
                  streaming={i === lastStepsIdx ? streaming : false}
                  turnStartMs={i === lastStepsIdx ? turnStartMs : null}
                  hasAnswer={
                    i === lastStepsIdx &&
                    message.content.length > 0 &&
                    message.stopReason == null &&
                    !isResumableStopNotice(message.content)
                  }
                  answer={
                    i === lastStepsIdx &&
                    message.stopReason == null &&
                    !isResumableStopNotice(message.content)
                      ? message.content
                      : undefined
                  }
                  // Per-segment expandId (#875): force-open THIS segment's
                  // StepGroup when a match lives in its tool step, without
                  // expanding unrelated segments of the same turn. `seg.key`
                  // is stable across renders (see `lib/turn-groups.ts`).
                  messageId={message.id}
                  segmentKey={seg.key}
                  onExportTimeline={
                    i === firstStepsIdx && exportEnabled
                      ? (format) =>
                          void downloadStepTimeline(
                            items,
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
              ),
            )
          )}
        </div>
      ) : (
        // No tool steps this turn: the Thinking block stands alone, but folded by
        // default (#205) so it stays compact.
        reasoning && (
          <div className="w-full">
            <ThinkingBlock
              reasoning={reasoning}
              streaming={streaming}
              hasAnswer={message.content.length > 0}
            />
          </div>
        )
      )}
      {message.content &&
        // A settled turn that stopped without a usable answer renders as a calm
        // Cancelled banner with an inline Continue button (#636) instead of the raw
        // marker text. Prefers the structured `stopReason` (#658); falls back to the
        // `[stopped…]` marker string for rows persisted before the structured column.
        // Only when settled — a stop can't legitimately appear mid-stream.
        (!streaming &&
        (message.stopReason != null ||
          isResumableStopNotice(message.content)) ? (
          <CancelledNotice
            sessionId={message.sessionId}
            content={message.content}
            stopReason={message.stopReason}
          />
        ) : (
          <div className="group relative w-full">
            {/* This iteration's own content, still streaming. Usually the
                growing final answer (rendered bare, as always) — but if it
                *already* has a tool call of its own recorded (`hasOwnLiveSteps`,
                never true on reload — `toolStepsByMessage` is a live-session-only
                map), it's guaranteed not the final answer, so it gets the same
                "On it" collapse as a settled intermediate-prose segment, just
                not-yet-settled (#864). A `prose` TurnItem for this same text
                won't exist until the *next* iteration starts, by which point
                it's already fully settled (see active-prose-block.tsx) — this
                is the only place the *live* in-flight case is reachable.
                Gated on `streaming` too: `toolStepsByMessage[m.id]` outlives
                the stream (cleared only on reload/edit-truncation), so once
                this message settles it must fall back to the plain branch
                below like any other finished answer. */}
            {hasOwnLiveSteps && streaming ? (
              <ActiveProseBlock
                text={message.content}
                streaming={streaming}
                tone="foreground"
                caret={streaming}
              />
            ) : (
              <div
                data-selectable
                className={cn(
                  "px-0.5 py-1 text-sm leading-relaxed text-foreground",
                  streaming && "ff-streaming-caret",
                )}
              >
                <Markdown content={message.content} streaming={streaming} />
              </div>
            )}
            {/* Always-visible copy affordance under the response (#604). Hidden
                mid-stream — copying a half-streamed answer is wrong. */}
            {!streaming && (
              <div className="mt-1 flex items-center px-0.5">
                <ResponseCopyButton text={message.content} />
              </div>
            )}
          </div>
        ))}
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
  // Scope these shared, session-keyed maps to THIS pane's messages so pane B's
  // ChatView doesn't re-render on every token pane A streams (#1009). `useShallow`
  // absorbs the new-top-level-ref-same-own-content case; narrowing to this
  // session's ids first keeps foreign keys out of the shallow comparison.
  const toolStepsByMessage = useChatStore(
    useShallow((s) =>
      scopeByIds(
        s.toolStepsByMessage,
        targetSessionId ? s.messagesBySession[targetSessionId] : undefined,
      ),
    ),
  );
  const turnStartByMessage = useChatStore(
    useShallow((s) =>
      scopeByIds(
        s.turnStartByMessage,
        targetSessionId ? s.messagesBySession[targetSessionId] : undefined,
      ),
    ),
  );
  const reasoningByMessage = useChatStore(
    useShallow((s) =>
      scopeByIds(
        s.reasoningByMessage,
        targetSessionId ? s.messagesBySession[targetSessionId] : undefined,
      ),
    ),
  );
  // Only this session's turn-start is ever read; a scalar selector mirrors the
  // `pending` one above and can't be perturbed by another pane's turn.
  const turnStartForSession = useChatStore((s) =>
    targetSessionId ? s.turnStartBySession[targetSessionId] : undefined,
  );
  // Dev step-timeline export (#417): the affordance shows only with the flag on; the
  // active model id is stamped into the dump's meta.
  const exportEnabled = useExperimentalStore((s) => s.flags.stepTimelineExport);
  const exportModel = useModelConfigStore(
    (s) => activeConnection(s.registry)?.model ?? null,
  );
  // Which user message (if any) is open in its bubble-anchored edit box (#929 A).
  // Pane-local: each ChatView edits at most one message at a time. The session id
  // is stored alongside so switching sessions *derives* a closed box rather than
  // resetting it from an effect — a stale id can never leak into another session.
  const [editing, setEditing] = useState<{
    sessionId: string | undefined;
    messageId: string;
  } | null>(null);
  const editingId =
    editing && editing.sessionId === targetSessionId ? editing.messageId : null;
  const beginEdit = useCallback(
    (messageId: string) =>
      setEditing({ sessionId: targetSessionId, messageId }),
    [targetSessionId],
  );
  const endEdit = useCallback(() => setEditing(null), []);

  const respondApproval = useChatStore((s) => s.respondApproval);
  const approveSession = useChatStore((s) => s.approveSession);
  const approveAlways = useChatStore((s) => s.approveAlways);
  const respondAsk = useChatStore((s) => s.respondAsk);

  // Fold the transcript into per-turn render groups (#413/#415). Steps are resolved
  // per assistant message: live `toolStepsByMessage` while streaming (aggregated
  // across every iteration of a multi-step turn), or reconstructed from the persisted
  // tool/system messages on reload. Intermediate prose interleaves as folded rows.
  //
  // Split at the active turn's boundary (#1022): a streamed token only mutates the
  // final turn, so folding the whole transcript every frame is O(transcript) per
  // frame → O(n²) over a turn, and the jank scales with length. Fold the immutable
  // prefix once (`usePrefixFold`, invalidated only when its messages actually change)
  // and re-fold just the tail — the active turn — each frame. `foldTurns` is
  // turn-boundaried, so concatenating the halves is identical to folding the whole
  // (asserted in turn-groups.test.ts).
  const msgs = messages ?? EMPTY_MESSAGES;
  const prefixEnd = useMemo(() => {
    const boundary = lastTurnStart(msgs);
    return boundary < 0 ? 0 : boundary;
  }, [msgs]);
  // The prefix is everything before the active turn; it is immutable while that turn
  // streams, so key its fold on the prefix's *identity* — the last committed message
  // ref (+ boundary + session) — not on `msgs` or the step/reasoning maps, all of
  // which get a fresh ref on every streamed token. This is what skips the O(prefix)
  // fold per frame. The key changes on exactly the cases that mutate the committed
  // transcript: a history reload / edit / truncate (message objects replaced) or a
  // session switch. A committed turn never gains new steps/reasoning — those flow only
  // to the streaming message — so the maps read at fold time are always current for
  // prefix ids even though they aren't in the dep list.
  const lastPrefixMsg = prefixEnd > 0 ? msgs[prefixEnd - 1] : undefined;
  const prefixGroups = useMemo(
    () =>
      foldTurns(
        msgs.slice(0, prefixEnd),
        toolStepsByMessage,
        reasoningByMessage,
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [targetSessionId, prefixEnd, lastPrefixMsg],
  );
  const tailGroups = useMemo(
    () =>
      foldTurns(msgs.slice(prefixEnd), toolStepsByMessage, reasoningByMessage),
    [msgs, prefixEnd, toolStepsByMessage, reasoningByMessage],
  );
  const groups = useMemo(
    () => [...prefixGroups, ...tailGroups],
    [prefixGroups, tailGroups],
  );

  // Retry is confined to the tail user message (#929 C): the backend truncates
  // everything after its target, so a mid-conversation retry would silently nuke
  // the rest of the thread. Earlier messages stay editable, just not retryable.
  const lastUserMessageId = useMemo(() => {
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === "user") return msgs[i].id;
    }
    return null;
  }, [msgs]);

  // While the in-thread find bar (#679) is open for this session, stop forcing
  // the transcript to the bottom so stepping through matches (scrollIntoView)
  // isn't yanked back down by streaming autoscroll. Normal pinning resumes on
  // close — this replaces having to reach into `pinnedToBottom` from the find UI.
  //
  // Tradeoff (PR #739 review): this suppresses stream-follow for the WHOLE time
  // find is open, not just the single jump — so a turn streaming with find open
  // won't auto-scroll to new tokens until the bar closes. Intentional for an
  // IDE-style find (the user is reading a match, not the tail); revisit if we
  // want to suppress only the one scroll-into-view instead of the whole stream.
  const findOn = useFindStore((s) => s.open && s.sessionId === targetSessionId);

  // Element *state*, not refs (#866): the transcript renders `null` until
  // `loadSession` resolves for a session with no localStorage cache, so the
  // scroll container mounts on a later commit than the one these effects first
  // ran on. A ref mutation doesn't re-run an effect, so with `useRef` the
  // observer below was never attached on that mount — no initial pin, and no
  // streaming follow either. A callback ref re-renders when the node appears,
  // which puts the mount itself in the dep list.
  const [scrollEl, setScrollEl] = useState<HTMLDivElement | null>(null);
  // The growing content wrapper, observed for post-layout height changes (#1025).
  const [contentEl, setContentEl] = useState<HTMLDivElement | null>(null);
  const pinnedToBottom = useRef(true);
  // Render-state mirror of `pinnedToBottom` so the floating "Jump to latest" button
  // (#206) shows only while scrolled up. Toggles at the same 40px threshold.
  const [atBottom, setAtBottom] = useState(true);

  // Stay pinned to the bottom while streaming, but respect manual scroll-up.
  // Pinning has to happen *after* the browser lays out the new content, not at
  // React commit time (#1025): a large markdown / <pre> / image block grows the
  // scroll container's height *after* the token that inserted it commits, so a
  // commit-time `scrollTop = scrollHeight` reads a pre-layout height and lands
  // the view above the real tail. A ResizeObserver fires post-layout for every
  // size change — each token, the pending indicator, late-settling blocks, an
  // image finishing load — and re-pins to the true bottom. Only follows while the
  // user is still pinned; `findOn` (#679) suppresses the yank while reading a
  // match. Writing `scrollTop` doesn't resize the content, so there's no loop.
  //
  // The observer can fire many times per frame during a fast stream (the #1022
  // churn cluster), so the pin is coalesced into a single rAF-scheduled write per
  // frame instead of a synchronous write per fire. The rAF also reads
  // `scrollHeight` at paint time — the freshest post-layout height for that frame.
  useEffect(() => {
    if (!scrollEl || !contentEl) return;
    let raf = 0;
    const ro = new ResizeObserver(() => {
      if (raf) return; // a pin is already scheduled for this frame
      raf = requestAnimationFrame(() => {
        raf = 0;
        if (pinnedToBottom.current && !findOn) {
          scrollEl.scrollTop = scrollEl.scrollHeight;
        }
      });
    });
    ro.observe(contentEl);
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [scrollEl, contentEl, findOn]);

  // A session swap can land on a transcript of the same height (no ResizeObserver
  // fire), so re-arm the pin and jump to the tail explicitly here. `findOn` gates
  // it so the in-thread find (#679) and its global-search seed (#710) aren't
  // yanked back to the tail the moment the session switches (#875).
  //
  // The pin is deferred to a rAF for the same post-layout reason as the observer
  // (#1025): at commit time the freshly mounted rows haven't laid out, so a
  // synchronous `scrollTop = scrollHeight` reads a short height and lands above
  // the tail.
  useEffect(() => {
    pinnedToBottom.current = true;
    if (!scrollEl || findOn) return;
    const raf = requestAnimationFrame(() => {
      scrollEl.scrollTop = scrollEl.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
  }, [targetSessionId, scrollEl, findOn]);

  // The relaunch swap (#866): `loadSession` replaces the store's <=50-message
  // localStorage tail with the backend's full history, which can render at the
  // *same* height (a session of <=50 messages) — so no ResizeObserver fire —
  // while `targetSessionId` never changed, so the effect above doesn't re-run
  // either. Pin on the transcript's identity changing as well, under exactly the
  // observer's conditions: only while the user is still pinned (a scroll-up has
  // already cleared `pinnedToBottom`, so this can't fight someone reading
  // history) and never while find is open. Content-driven rather than
  // size-driven, which is what covers the equal-height swap.
  useEffect(() => {
    if (!scrollEl || findOn) return;
    const raf = requestAnimationFrame(() => {
      if (pinnedToBottom.current) scrollEl.scrollTop = scrollEl.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
  }, [messages, scrollEl, findOn]);

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
    const el = scrollEl;
    if (!el) return;
    const pinned = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    pinnedToBottom.current = pinned;
    setAtBottom(pinned);
  }

  // Smooth-scroll to the newest content and re-arm sticky autoscroll (#206).
  function jumpToLatest() {
    const el = scrollEl;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    pinnedToBottom.current = true;
    setAtBottom(true);
  }

  // Distinguish "not loaded yet" from "genuinely empty session" (#785):
  // messagesBySession[id] is undefined until loadSession() completes, and []
  // once it resolves (even for a fresh draft). Showing the empty-state prompt
  // during the load window causes a ~0.5s flash on cold start (#599 regression).
  if (messages === undefined) return null;
  if (messages.length === 0) {
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
        ref={setScrollEl}
        onScroll={handleScroll}
        data-testid="chat-scroll"
        className="min-h-0 flex-1 overflow-y-auto"
      >
        <div
          ref={setContentEl}
          className="mx-auto flex max-w-4xl flex-col gap-3 px-4 py-4"
        >
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
                hasOwnLiveSteps={liveTiming}
                items={items}
                turnStartMs={turnStartByMessage[m.id] ?? turnStartForSession}
                reasoning={reasoning}
                exportEnabled={exportEnabled}
                exportModel={exportModel}
                exportTiming={liveTiming ? "exact" : "approx-created-at"}
                isEditing={m.id === editingId}
                isLastUserMessage={m.id === lastUserMessageId}
                beginEdit={beginEdit}
                endEdit={endEdit}
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
