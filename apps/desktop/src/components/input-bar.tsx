import { useCallback, useEffect, useRef } from "react";
import { ArrowUp, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { warmupTrigger } from "@/lib/warmup";

export function InputBar() {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  // Composer text lives in a shared store so "edit & resend" (Issue #18) can
  // prefill it from a message row without prop-drilling.
  const value = useComposerStore((s) => s.text);
  const setText = useComposerStore((s) => s.setText);
  const focusNonce = useComposerStore((s) => s.focusNonce);
  const rejectNonce = useComposerStore((s) => s.rejectNonce);

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const streaming = useChatStore((s) =>
    s.activeSessionId
      ? Boolean(s.streamingBySession[s.activeSessionId])
      : false,
  );
  const send = useChatStore((s) => s.send);
  const cancelActiveTurn = useChatStore((s) => s.cancelActiveTurn);

  const autoGrow = useCallback((el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, []);

  // Keyboard-native: focus follows the active session.
  useEffect(() => {
    textareaRef.current?.focus();
  }, [activeSessionId]);

  // Edit & resend prefills the text and bumps focusNonce; focus, grow, and drop
  // the caret at the end here — all DOM, no state set in the effect.
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.focus();
    autoGrow(el);
    el.setSelectionRange(el.value.length, el.value.length);
  }, [focusNonce, autoGrow]);

  // A refused prefill (#48) kept an in-progress draft instead of clobbering it —
  // shake the composer and refocus so the action isn't silently ignored. DOM
  // only, no state set in the effect. (rejectNonce starts at 0; skip that.)
  useEffect(() => {
    if (rejectNonce === 0) return;
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
    if (!content || streaming || !activeSessionId) return;
    setText("");
    // Collapse the box back to one line (it may have grown for a resend draft).
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    void send(content);
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
          value={value}
          rows={1}
          placeholder="Message FlowForge…"
          className="max-h-40 min-h-8 flex-1 resize-none bg-transparent px-2 py-1.5 text-[13px] leading-relaxed placeholder:text-muted-foreground/50 focus-visible:outline-none"
          onFocus={() => warmupTrigger.fire()}
          onChange={(e) => {
            warmupTrigger.fire();
            setText(e.currentTarget.value);
            autoGrow(e.currentTarget);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
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
            onClick={() => void cancelActiveTurn()}
            title="Stop (Esc)"
          >
            <Square className="size-3.5" />
          </Button>
        ) : (
          <Button
            size="icon"
            className="size-8 shrink-0 rounded-lg"
            disabled={!value.trim() || !activeSessionId}
            onClick={submit}
            title="Send (Enter)"
          >
            <ArrowUp className="size-4" />
          </Button>
        )}
      </div>
    </div>
  );
}
