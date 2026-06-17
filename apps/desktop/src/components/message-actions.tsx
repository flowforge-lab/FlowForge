import { useState, type ReactNode } from "react";
import { Check, Copy, PencilLine } from "lucide-react";
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

// Mirrors the clipboard + transient-"Copied" pattern from markdown.tsx's CopyButton.
function CopyAction({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <ActionButton
      title={copied ? "Copied" : "Copy"}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // Clipboard unavailable (permissions / insecure context); fail quiet.
        }
      }}
    >
      {copied ? (
        <Check className="size-3 text-emerald-500" />
      ) : (
        <Copy className="size-3" />
      )}
    </ActionButton>
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
  const prefill = useComposerStore((s) => s.prefill);
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
          onClick={() => prefill(message.sessionId, message.content)}
        >
          <PencilLine className="size-3" />
        </ActionButton>
      )}
    </div>
  );
}
