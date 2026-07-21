import { type ReactNode } from "react";
import { Check, Copy, PencilLine, RotateCcw } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useCopied } from "@/lib/use-copied";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

function ActionButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      onClick={onClick}
      className="flex size-6 items-center justify-center rounded text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
    >
      {children}
    </button>
  );
}

// Mirrors the clipboard + transient-"Copied" pattern via the shared `useCopied`.
function CopyAction({ text }: { text: string }) {
  const { copied, copy } = useCopied();
  return (
    <ActionButton
      title={copied ? "Copied" : "Copy"}
      onClick={() => void copy(text)}
    >
      {copied ? (
        <Check className="size-3 text-emerald-500" />
      ) : (
        <Copy className="size-3" />
      )}
    </ActionButton>
  );
}

// Labeled, flow-positioned copy button for the assistant answer footer (#604).
// Unlike the margin `CopyAction`, this sits in normal document flow directly under
// the response and is visible by default (dimmed), strengthening on hover/focus via
// the parent `group`. Icon + text label mirrors markdown.tsx's CopyButton so it
// reads obviously as a button.
export function ResponseCopyButton({ text }: { text: string }) {
  const { copied, copy } = useCopied();
  return (
    <button
      type="button"
      onClick={() => void copy(text)}
      title={copied ? "Copied" : "Copy"}
      className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground/80 opacity-70 transition-all hover:bg-foreground/10 hover:text-foreground group-hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
    >
      {copied ? (
        <Check className="size-3 text-emerald-500" />
      ) : (
        <Copy className="size-3" />
      )}
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

// Per-message hover/focus toolbar (Issue #18). Absolutely positioned beside the
// bubble so revealing it never shifts layout; buttons are real <button>s, so they
// stay reachable via keyboard (the parent reveals on group-focus-within too).
export function MessageActions({
  message,
  side,
  isLastUserMessage = false,
  onBeginEdit,
}: {
  message: Message;
  side: "left" | "right";
  /** Gates Retry (#929 C): only the tail user message can re-run without
   *  collateral, since the backend truncates everything after the target. */
  isLastUserMessage?: boolean;
  /** Opens the bubble-anchored edit box (#929 A). Owned by chat-view. */
  onBeginEdit?: () => void;
}) {
  const editMessage = useChatStore((s) => s.editMessage);
  return (
    <div
      className={cn(
        "absolute top-1 flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100",
        side === "left" ? "right-full mr-1" : "left-full ml-1",
      )}
    >
      <CopyAction text={message.content} />
      {message.role === "user" && (
        <ActionButton title="Edit & resend" onClick={() => onBeginEdit?.()}>
          <PencilLine className="size-3" />
        </ActionButton>
      )}
      {/* Retry (#929 C) — tail user message only. Re-sends the same input through
          the existing edit path, so the backend truncates and re-runs: the current
          answer is REPLACED, not kept alongside. That's intentional (FlowForge has
          no variant model), and the title says so rather than implying a <1/2>
          switcher. Attachments pass through — the backend replaces the column
          wholesale, so dropping them here would destroy them. */}
      {message.role === "user" && isLastUserMessage && (
        <ActionButton
          title="Retry — replaces the current answer"
          onClick={() =>
            void editMessage(
              message.sessionId,
              message.id,
              message.content,
              message.attachments ?? undefined,
            )
          }
        >
          <RotateCcw className="size-3" />
        </ActionButton>
      )}
    </div>
  );
}
