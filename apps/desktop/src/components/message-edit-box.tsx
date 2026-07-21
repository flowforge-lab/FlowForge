import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";

/**
 * Bubble-anchored, contained edit box for a user message (#929 part A).
 *
 * Replaces the old flow, which routed the message text into the SHARED main
 * composer behind a thin banner — the user could not tell whether they were
 * editing an existing message or composing a new one. This box renders in place
 * of the bubble it belongs to, so the edit is unmistakably attached to that
 * message, and it carries visible Save / Cancel buttons (Enter and Esc stay as
 * accelerators, never the only way).
 *
 * Saving is destructive by design: FlowForge's `edit_user_message` UPDATEs the
 * row in place and DELETEs everything after it — truncate-and-rerun, no
 * branching. The inline note says so; there is no branch/arrow UI to build.
 *
 * Owns no store state: the parent supplies the seed text and handles `onSave` /
 * `onCancel`.
 */
export function MessageEditBox({
  initialText,
  onSave,
  onCancel,
}: {
  initialText: string;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initialText);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const autoGrow = useCallback((el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, []);

  // Focus with the caret at the end so the user types into their own text
  // rather than over it, and size the box to the seed content on open.
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    autoGrow(el);
  }, [autoGrow]);

  // Blank is not a message; an unchanged text still submits (a deliberate re-run).
  const canSave = text.trim().length > 0;

  return (
    <div className="flex w-full flex-col gap-2 rounded-2xl rounded-br-md border border-border bg-background px-3 py-2 shadow-sm focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/40">
      <textarea
        ref={textareaRef}
        aria-label="Edit message"
        value={text}
        rows={1}
        className="max-h-40 w-full resize-none bg-transparent text-[15px] leading-relaxed focus-visible:outline-none"
        onChange={(e) => {
          setText(e.currentTarget.value);
          autoGrow(e.currentTarget);
        }}
        onKeyDown={(e) => {
          // Esc abandons the edit; stop propagation so the shell's global Esc
          // (cancel the active turn, app-shell.tsx) doesn't also fire —
          // abandoning an edit must not kill an in-flight turn.
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onCancel();
            return;
          }
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            if (canSave) onSave(text);
          }
        }}
      />
      <p className="text-[11px] leading-snug text-muted-foreground">
        Saving replaces this message and re-runs — the responses below it are
        discarded.
      </p>
      <div className="flex items-center justify-end gap-1.5">
        <Button type="button" variant="ghost" size="xs" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          type="button"
          variant="default"
          size="xs"
          disabled={!canSave}
          onClick={() => onSave(text)}
        >
          Save
        </Button>
      </div>
    </div>
  );
}
