import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useVirtualizer } from "@tanstack/react-virtual";
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
import type { RenderGroup, TurnItem } from "@/lib/turn-groups";
import { useExperimentalStore } from "@/store/experimental";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { useModelConfigStore, activeConnection } from "@/store/model-config";
import { downloadStepTimeline } from "@/lib/export-step-timeline";
import type { Message } from "@/bindings";

const NO_STEPS: ToolStep[] = [];
const NO_ITEMS: TurnItem[] = [];
// #866: how long a session switch's forced bottom-pin stays armed, bridging
// the async gap until loadSession()'s full-history swap-in settles.
//
// 4s is ~40x the observed local round trip (one `get_messages` IPC plus a
// SQLite read of a few hundred rows, single-digit-to-tens of ms warm; the
// slowest cold relaunch measured against a 734-message session was still well
// under 100ms), so it is headroom, not a deadline anyone is expected to
// approach. A bound is needed at all because the arm's proof — the transcript's
// first message id changing — stays true *forever* after the swap: the armed
// baseline is the cached head, so without an expiry a swap that resizes nothing
// (a session at or under the 50-message cache cap, where cached tail and full
// history render identically) would leave the arm live until some unrelated
// later resize, and force-pin *that* over a deliberate scroll-up. The two
// failure modes are therefore symmetric and unavoidable in this shape: too
// short silently degrades to today's behavior (#866 returns for that load), too
// long yanks a reading user once. This errs toward the yank being impossible
// and the miss being implausible.
const FORCE_PIN_WINDOW_MS = 4000; // generous margin over a local IPC round trip
// Stable empty transcript so a not-yet-loaded session keeps a constant `msgs` ref.
const EMPTY_MESSAGES: Message[] = [];

// --- Virtualized transcript (#1143) ------------------------------------------
// Every row's real height is measured (`measureElement`); this is only the
// first-paint guess for rows that haven't mounted yet, and it decides how far
// the scrollbar lies before they do. Roughly a short user bubble plus its
// header — deliberately on the low side, since underestimating renders a few
// extra rows (cheap) while overestimating leaves a gap at the tail (visible).
const ROW_ESTIMATE_PX = 140;
// Rows kept mounted beyond each edge of the viewport. Enough that a flick-scroll
// doesn't expose unmeasured space, small enough that the DOM stays bounded —
// this constant is what the node-count invariant in
// `chat-view.virtualization.test.tsx` is really asserting about.
const OVERSCAN = 6;
// Viewport assumed for the frame between mount and the virtualizer's first
// measurement, so the initial window isn't computed against a 0×0 box. The real
// size replaces it as soon as the scroll element is observed — this only has to
// be the right order of magnitude. (It does NOT rescue jsdom, which reports
// `offsetHeight: 0`: that measurement lands immediately and wins. Tests that
// need a viewport stub `offsetWidth`/`offsetHeight` — see
// `chat-view.virtualization.test.tsx`.)
const INITIAL_RECT = { width: 800, height: 1000 };

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
  // Bridges the async gap between the session-switch effect's pin to the cached
  // tail and loadSession()'s later swap-in of the full history
  // (chat.ts:457-490). During that gap, the browser's default CSS scroll
  // anchoring (no overflow-anchor:none in this app) can fire a `scroll` event
  // while older messages are inserted above the preserved tail, flipping
  // `pinnedToBottom.current` to false before either pin path below checks
  // it (#866) — both of them gate on that flag, so both consult the arm.
  //
  // A bare time window isn't enough to gate the override: ordinary streaming
  // (new tokens, a late-settling markdown block) also fires the same
  // ResizeObserver shortly after a switch, and forcing *that* through would
  // override a genuine manual scroll-up — regressing the "scroll-up detaches
  // autoscroll" behavior for several seconds after every switch. So the
  // override additionally requires proof that a history swap-in actually
  // happened: `loadSession` *prepends* older messages above the preserved
  // tail, changing the transcript's first message id — something ordinary
  // streaming (which only appends or mutates the last message) never does.
  // `latestMessagesRef` mirrors the current transcript every render so the
  // ResizeObserver closure (deliberately NOT re-created per message, to avoid
  // recreating the observer per streamed token, #1022) can read it without
  // being in the effect's dependency array.
  const forcePinUntil = useRef(0);
  const armedFirstMessageId = useRef<string | undefined>(undefined);
  const latestMessagesRef = useRef(messages);
  useEffect(() => {
    latestMessagesRef.current = messages;
  }, [messages]);

  // THE pin decision — the single implementation every trigger funnels through
  // (the ResizeObserver settle, the session switch, and the transcript swap), so
  // there is one gate and one place the arm is read and consumed, not a copy per
  // trigger. Returns whether the caller should pin; the caller does its own
  // one-line `scrollTop = scrollHeight` (writing to the container from in here
  // would be mutating a `useState` value inside a callback, which the compiler
  // lint rejects). Always call it from inside a rAF: every caller needs the
  // post-layout height, for the reasons #1025 documents on the observer.
  //
  // `authoritative` marks the ResizeObserver settle — the only caller that can
  // know layout finished for this frame, and therefore the only one allowed to
  // consume the arm. The other callers must NOT consume: they run on the commit
  // that swaps the history in, which is *before* scroll anchoring fires its
  // intermediate `scroll` event, so consuming there would spend the arm on the
  // pre-race pin and leave the post-race settle ungated — verified by flipping
  // this one flag, which reproduces the swap test's 2000-instead-of-4000.
  const shouldPinToTail = useCallback(
    (authoritative: boolean) => {
      if (findOn) return false;
      // The arm only overrides a *transient* false, and only on proof that the
      // armed switch's history swap-in actually landed (first message id
      // changed) — never on ordinary streaming growth.
      const historySwapped =
        forcePinUntil.current > Date.now() &&
        latestMessagesRef.current?.[0]?.id !== armedFirstMessageId.current;
      if (!pinnedToBottom.current && !historySwapped) return false;
      if (historySwapped && authoritative) {
        forcePinUntil.current = 0; // consume: one forced settle per arm
        pinnedToBottom.current = true;
        setAtBottom(true);
      }
      return true;
    },
    [findOn],
  );

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
        // The authoritative post-layout settle — the one caller that consumes.
        if (shouldPinToTail(true)) scrollEl.scrollTop = scrollEl.scrollHeight;
      });
    });
    ro.observe(contentEl);
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [scrollEl, contentEl, shouldPinToTail]);

  // A session swap can land on a transcript of the same height (no ResizeObserver
  // fire), so re-arm the pin and jump to the tail explicitly here. `findOn` gates
  // it so the in-thread find (#679) and its global-search seed (#710) aren't
  // yanked back to the tail the moment the session switches (#875).
  //
  // The pin is deferred to a rAF for the same post-layout reason as the observer
  // (#1025): at commit time the freshly mounted rows haven't laid out, so a
  // synchronous `scrollTop = scrollHeight` reads a short height and lands above
  // the tail.
  //
  // It also only pins to whatever is rendered *right now* — on a cold session
  // switch that's the short localStorage-cached tail (message-cache.ts), not
  // the full history loadSession() swaps in moments later (#866). Arm
  // `forcePinUntil`/`armedFirstMessageId` here so `shouldPinToTail` can force
  // the eventual full-history settle to the true bottom, even if a transient
  // scroll event flips `pinnedToBottom.current` to false in between.
  useEffect(() => {
    pinnedToBottom.current = true;
    forcePinUntil.current = Date.now() + FORCE_PIN_WINDOW_MS;
    armedFirstMessageId.current = messages?.[0]?.id;
    if (!scrollEl) return;
    const raf = requestAnimationFrame(() => {
      if (shouldPinToTail(false)) scrollEl.scrollTop = scrollEl.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
    // `messages` is deliberately NOT a dependency: the arm must capture
    // whatever is rendered at the moment of the switch (the cache), not re-arm
    // on every later token — `latestMessagesRef` is what `shouldPinToTail`
    // reads live, for exactly that reason.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetSessionId, scrollEl, shouldPinToTail]);

  // The relaunch swap (#866): `loadSession` replaces the store's <=50-message
  // localStorage tail with the backend's full history, which can render at the
  // *same* height (a session of <=50 messages) — so no ResizeObserver fire —
  // while `targetSessionId` never changed, so the effect above doesn't re-run
  // either. Pin on the transcript's identity changing as well, through the same
  // `shouldPinToTail` gate; non-authoritative, so it never consumes the arm.
  useEffect(() => {
    if (!scrollEl) return;
    const raf = requestAnimationFrame(() => {
      if (shouldPinToTail(false)) scrollEl.scrollTop = scrollEl.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
  }, [messages, scrollEl, shouldPinToTail]);

  // Reset the scroll affordance when the pane switches sessions. setState during
  // render is React's recommended reset-on-prop-change pattern — no effect, no
  // flash of the button before a scroll event would otherwise clear it. (The
  // `pinnedToBottom` ref is re-armed by the session-switch effect above.)
  const [renderedSession, setRenderedSession] = useState(targetSessionId);
  if (renderedSession !== targetSessionId) {
    setRenderedSession(targetSessionId);
    setAtBottom(true);
  }

  // Windowed rendering (#1143). Mounting every row costs ~0.3ms each, which on a
  // long session dominates everything else by two orders of magnitude (2153ms of
  // a ~2.2s session open, against ~43ms for the entire load path). The full
  // session stays in the store — only the DOM is windowed — so nothing that
  // reads `messages` changes.
  //
  // What that does NOT buy: anything that reads the rendered DOM. The in-thread
  // find bar walks `[data-message-id]` nodes to build paintable ranges, so a
  // complete store is not enough — it has to ask for a row to be mounted before
  // it can range over it (`store/transcript-scroll.ts`, registered below).
  //
  // Behind a flag while the scroll machinery (#206/#866/#1025) is dogfooded
  // against it on a real large database; `virtualItems` is empty when off and
  // the plain list renders instead.
  const virtualized = useExperimentalStore(
    (s) => s.flags.virtualizedTranscript,
  );
  // The virtualizer hands back fresh closures every render, so the React
  // compiler skips memoizing this component. Accepted: it manages its own
  // subscriptions, nothing here holds one of those functions across renders, and
  // the per-row `MessageRow` memo — which is what actually keeps a streamed
  // token from re-rendering the transcript — is unaffected.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: virtualized ? groups.length : 0,
    getScrollElement: () => scrollEl,
    estimateSize: () => ROW_ESTIMATE_PX,
    // Key by message id, not index: rows are inserted at the *front* by the
    // #866 history swap, and an index key would remap every measured height to
    // the wrong row when that lands.
    getItemKey: (i) => groups[i].message.id,
    overscan: OVERSCAN,
    initialRect: INITIAL_RECT,
  });
  const virtualItems = virtualizer.getVirtualItems();

  // Publish a reveal for this session while windowing is on (#1143). Anything
  // that needs to look at a specific message — the find bar stepping onto a hit
  // far above the viewport — can only do so once the row is mounted, and a
  // windowed list is the only thing that can mount it. Registered ONLY on the
  // virtual path: on the plain path every row is already in the DOM, and
  // `reveal()` returning false is the correct "nothing to do" answer there.
  //
  // `groups` is read through a ref rather than captured, so the registration
  // doesn't churn on every streamed token (the same reason `latestMessagesRef`
  // exists for the pin).
  const groupsRef = useRef(groups);
  useEffect(() => {
    groupsRef.current = groups;
  }, [groups]);
  const register = useTranscriptScroll((s) => s.register);
  useEffect(() => {
    if (!virtualized || !targetSessionId) return;
    return register(targetSessionId, (messageId) => {
      const index = groupsRef.current.findIndex(
        (g) => g.message.id === messageId,
      );
      if (index < 0) return false;
      // `center` so a hit lands mid-viewport with context either side, matching
      // what `scrollRangeIntoView` does on the non-virtual path.
      virtualizer.scrollToIndex(index, { align: "center" });
      return true;
    });
    // `virtualizer` is a stable instance for the life of the component; adding it
    // would re-register on every render for no gain.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [virtualized, targetSessionId, register]);

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

  // One row, rendered identically by both the plain and the virtualized list —
  // the two paths differ only in *which* rows they ask for and how they're
  // positioned, never in what a row is.
  const renderGroup = (g: RenderGroup) => {
    const m = g.message;
    // foldTurns already resolved each turn's steps (live or reconstructed)
    // and interleaved prose items (#415).
    const toolSteps = g.kind === "assistant" ? g.steps : NO_STEPS;
    const items = g.kind === "assistant" ? g.items : NO_ITEMS;
    // Prefer live reasoning when present; fall back to the reasoning
    // persisted on the assistant message for a reloaded turn (#375).
    const reasoning =
      reasoningByMessage[m.id] ?? (g.kind === "assistant" ? g.reasoning : "");
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
  };

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
          className={cn(
            "mx-auto w-full max-w-4xl px-4 py-4",
            !virtualized && "flex flex-col gap-3",
          )}
        >
          {virtualized ? (
            // The spacer carries the full measured height so the scrollbar is
            // honest about the whole session; rows are absolutely positioned
            // inside it. `gap-3` can't survive absolute positioning, so the
            // per-row `pb-3` reproduces it — measured as part of the row, which
            // is what keeps the offsets consistent.
            <div
              className="relative w-full"
              style={{ height: virtualizer.getTotalSize() }}
            >
              {virtualItems.map((vi) => (
                <div
                  key={vi.key}
                  data-index={vi.index}
                  ref={virtualizer.measureElement}
                  className="absolute left-0 top-0 w-full pb-3"
                  style={{ transform: `translateY(${vi.start}px)` }}
                >
                  {renderGroup(groups[vi.index])}
                </div>
              ))}
            </div>
          ) : (
            groups.map((g) => renderGroup(g))
          )}
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
