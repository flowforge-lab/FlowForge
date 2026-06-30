import { useState, type ReactNode } from "react";
import { Check, Copy, PencilLine } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useComposerStore } from "@/store/composer";
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

// Clipboard + transient-"Copied" (1500ms) state, shared by the copy affordances
// here. Mirrors the same pattern in markdown.tsx's CopyButton: write to the
// clipboard, flash "Copied", then revert. Fail-quiet — the clipboard can be
// unavailable in an insecure context or without permission.
function useCopied() {
  const [copied, setCopied] = useState(false);
  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable (permissions / insecure context); fail quiet.
    }
  };
  return { copied, copy };
}

// Mirrors the clipboard + transient-"Copied" pattern from markdown.tsx's CopyButton.
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
}: {
  message: Message;
  side: "left" | "right";
}) {
  const beginEdit = useComposerStore((s) => s.beginEdit);
  return (
    <div
      className={cn(
        "absolute top-1 flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100",
        side === "left" ? "right-full mr-1" : "left-full ml-1",
      )}
    >
      <CopyAction text={message.content} />
      {message.role === "user" && (
        <ActionButton
          title="Edit & resend"
          onClick={() =>
            beginEdit(message.sessionId, message.id, message.content)
          }
        >
          <PencilLine className="size-3" />
        </ActionButton>
      )}
    </div>
  );
}
