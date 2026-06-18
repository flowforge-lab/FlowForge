import { useCallback, useEffect, useRef } from "react";
import { ArrowUp, Loader2, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";

// A local model server (candle-vllm, Ollama, …) clocks its GPU down when idle,
// so the first token after a pause crawls while the device ramps back up. We
// nudge it (`ipc.warmup`) while the user interacts with the composer — on focus
// and as they type — so the device is at full clock by the time they hit send
// and the first real token streams immediately.
//
// Measured on Apple Silicon: warmth decays ~7-10s after activity, so the
// throttle sits just under that window. When already warm a nudge is ~0.4s of
// GPU; cold, it absorbs the ramp the real turn would otherwise pay.
const WARMUP_THROTTLE_MS = 5_000;

// `sessionId` scopes the composer to one session so split panes (#148) each keep
// an independent draft and Stop/send target their own session. Defaults to the
// active session for the single-pane layout.
export function InputBar({
  sessionId,
  focused = true,
}: { sessionId?: string; focused?: boolean } = {}) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  // Read inside the focus effects without making them depend on `focused`: only
  // the focused pane (#148) should grab the composer, but merely focusing a pane
  // (e.g. selecting transcript text) must not yank the caret into its textarea.
  // Synced in an effect (not during render) so the focus effects below — which
  // run later in declaration order — always see the current value.
  const focusedRef = useRef(focused);
  useEffect(() => {
    focusedRef.current = focused;
  });

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const targetSessionId = sessionId ?? activeSessionId ?? undefined;

  // Composer text lives in a per-session store so "edit & resend" (Issue #18) can
  // prefill it from a message row without prop-drilling, and each pane (#148)
  // keeps its own draft.
  const value = useComposerStore((s) =>
    targetSessionId ? (s.textBySession[targetSessionId] ?? "") : "",
  );
  const setTextFor = useComposerStore((s) => s.setText);
  const focusNonce = useComposerStore((s) =>
    targetSessionId ? (s.focusNonceBySession[targetSessionId] ?? 0) : 0,
  );
  const rejectNonce = useComposerStore((s) =>
    targetSessionId ? (s.rejectNonceBySession[targetSessionId] ?? 0) : 0,
  );
  const setText = useCallback(
    (text: string) => {
      if (targetSessionId) setTextFor(targetSessionId, text);
    },
    [targetSessionId, setTextFor],
  );

  const streaming = useChatStore((s) =>
    targetSessionId ? Boolean(s.streamingBySession[targetSessionId]) : false,
  );
  // The gap between hitting send and the first streamed token: the turn is
  // in flight on the backend but nothing renders yet. Derive it from the
  // existing timing/streaming maps (turn started, no tokens) so the send
  // button can show a spinner instead of looking idle.
  const pending = useChatStore((s) =>
    targetSessionId
      ? Boolean(s.turnStartBySession[targetSessionId]) &&
        !s.streamingBySession[targetSessionId]
      : false,
  );
  const send = useChatStore((s) => s.send);
  const cancelTurn = useChatStore((s) => s.cancelTurn);
  const sendMessageKey = usePrefsStore((s) => s.sendMessageKey);

  const autoGrow = useCallback((el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, []);

  // Throttled, fire-and-forget server warmup (see note at top of file).
  const lastWarmupRef = useRef(0);
  const warmup = useCallback(() => {
    const now = Date.now();
    if (now - lastWarmupRef.current < WARMUP_THROTTLE_MS) return;
    lastWarmupRef.current = now;
    void ipc.warmup().catch(() => {});
  }, []);

  // Keyboard-native: focus follows the (pane's) session — but only for the
  // focused pane, so background panes don't steal focus on mount.
  useEffect(() => {
    if (focusedRef.current) textareaRef.current?.focus();
  }, [targetSessionId]);

  // Edit & resend prefills the text and bumps focusNonce; focus, grow, and drop
  // the caret at the end here — all DOM, no state set in the effect.
  useEffect(() => {
    const el = textareaRef.current;
    if (!el || !focusedRef.current) return;
    el.focus();
    autoGrow(el);
    el.setSelectionRange(el.value.length, el.value.length);
  }, [focusNonce, autoGrow]);

  // A refused prefill (#48) kept an in-progress draft instead of clobbering it —
  // shake the composer and refocus so the action isn't silently ignored. DOM
  // only, no state set in the effect. (rejectNonce starts at 0; skip that.)
  useEffect(() => {
    if (rejectNonce === 0 || !focusedRef.current) return;
    boxRef.current?.animate(
      [
        { transform: "translateX(0)" },
        { transform: "translateX(-4px)" },
        { transform: "translateX(4px)" },
        { transform: "translateX(-3px)" },
        { transform: "translateX(0)" },
      ],
      { duration: 350, easing: "ease-in-out" },
    );
    textareaRef.current?.focus();
  }, [rejectNonce]);

  function submit() {
    const content = value.trim();
    if (!content || streaming || pending || !targetSessionId) return;
    setText("");
    // Collapse the box back to one line (it may have grown for a resend draft).
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    void send(content, targetSessionId);
  }

  return (
    <div className="px-4 pb-4 pt-2">
      <div
        ref={boxRef}
        className="mx-auto flex max-w-3xl items-end gap-1.5 rounded-xl border bg-card p-1.5 shadow-sm transition-all focus-within:border-ring focus-within:shadow-md focus-within:ring-2 focus-within:ring-ring/25"
      >
        <textarea
          ref={textareaRef}
          data-composer
          data-pane-focused={focused ? "" : undefined}
          value={value}
          rows={1}
          placeholder="Message FlowForge…"
          className="max-h-40 min-h-8 flex-1 resize-none bg-transparent px-2 py-1.5 text-[13px] leading-relaxed placeholder:text-muted-foreground/50 focus-visible:outline-none"
          onFocus={warmup}
          onChange={(e) => {
            warmup();
            setText(e.currentTarget.value);
            autoGrow(e.currentTarget);
          }}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            // Enter mode: plain Enter sends (Shift+Enter = new line, unchanged).
            // Ctrl+Enter mode: Ctrl/⌘+Enter sends; any other Enter is a new line.
            const sends =
              sendMessageKey === "ctrlEnter"
                ? e.metaKey || e.ctrlKey
                : !e.shiftKey;
            if (sends) {
              e.preventDefault();
              submit();
            }
          }}
        />
        {streaming ? (
          <Button
            variant="outline"
            size="icon"
            className="size-8 shrink-0 rounded-lg"
            onClick={() => targetSessionId && void cancelTurn(targetSessionId)}
            title="Stop (Esc)"
          >
            <Square className="size-3.5" />
          </Button>
        ) : pending ? (
          <Button
            size="icon"
            className="size-8 shrink-0 rounded-lg"
            disabled
            title="Sending…"
            aria-label="Sending"
          >
            <Loader2 className="size-4 animate-spin" />
          </Button>
        ) : (
          <Button
            size="icon"
            className="size-8 shrink-0 rounded-lg"
            disabled={!value.trim() || !targetSessionId}
            onClick={submit}
            title={
              sendMessageKey === "ctrlEnter"
                ? "Send (⌘/Ctrl+Enter)"
                : "Send (Enter)"
            }
          >
            <ArrowUp className="size-4" />
          </Button>
        )}
      </div>
    </div>
  );
}
