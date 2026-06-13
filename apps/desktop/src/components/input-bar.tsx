import { useEffect, useRef, useState } from "react";
import { ArrowUp, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/store/chat";

export function InputBar() {
  const [value, setValue] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const streaming = useChatStore((s) =>
    s.activeSessionId
      ? Boolean(s.streamingBySession[s.activeSessionId])
      : false,
  );
  const send = useChatStore((s) => s.send);
  const cancelActiveTurn = useChatStore((s) => s.cancelActiveTurn);

  // Keyboard-native: focus follows the active session.
  useEffect(() => {
    textareaRef.current?.focus();
  }, [activeSessionId]);

  function submit() {
    const content = value.trim();
    if (!content || streaming || !activeSessionId) return;
    setValue("");
    void send(content);
  }

  function autoGrow(el: HTMLTextAreaElement) {
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }

  return (
    <div className="px-4 pb-4 pt-2">
      <div className="mx-auto flex max-w-3xl items-end gap-1.5 rounded-xl border bg-card p-1.5 shadow-sm transition-all focus-within:border-ring focus-within:shadow-md focus-within:ring-2 focus-within:ring-ring/25">
        <textarea
          ref={textareaRef}
          data-composer
          value={value}
          rows={1}
          placeholder="Message FlowForge…"
          className="max-h-40 min-h-8 flex-1 resize-none bg-transparent px-2 py-1.5 text-[13px] leading-relaxed placeholder:text-muted-foreground/50 focus-visible:outline-none"
          onChange={(e) => {
            setValue(e.currentTarget.value);
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
