import { useState } from "react";
import { Check, Copy, WrapText, X } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { HighlightedCode } from "@/components/markdown";
import {
  clampSplitWidth,
  useSplitStore,
  type SplitContent,
} from "@/store/split";

// ── Header controls ──────────────────────────────────────────────────────────

function IconButton({
  onClick,
  title,
  active,
  children,
}: {
  onClick: () => void;
  title: string;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className={cn(
        "flex size-6 items-center justify-center rounded transition-colors",
        active
          ? "bg-foreground/10 text-foreground"
          : "text-muted-foreground hover:bg-foreground/10 hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

function CopyIconButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <IconButton
      title={copied ? "Copied" : "Copy"}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // Clipboard unavailable (permissions / insecure context); fail quiet.
        }
      }}
    >
      {copied ? (
        <Check className="size-3.5 text-emerald-500" />
      ) : (
        <Copy className="size-3.5" />
      )}
    </IconButton>
  );
}

// ── Body ─────────────────────────────────────────────────────────────────────

function headerTitle(content: SplitContent): string {
  if (content.title) return content.title;
  return content.kind === "code" ? content.lang || "code" : "output";
}

function SplitBody({
  content,
  wrap,
}: {
  content: SplitContent;
  wrap: boolean;
}) {
  const preClass = cn(
    "min-h-full px-4 py-3 font-mono text-[12.5px] leading-relaxed text-foreground/90",
    wrap ? "whitespace-pre-wrap break-words" : "whitespace-pre",
  );

  switch (content.kind) {
    case "code":
      return (
        <pre className={preClass}>
          <HighlightedCode lang={content.lang} text={content.text} />
        </pre>
      );
    case "text":
      return <pre className={preClass}>{content.text}</pre>;
    default: {
      // Exhaustiveness guard: adding a SplitContent kind without a case here
      // becomes a compile error (the TODO finds you). See store/split.ts.
      const unreachable: never = content;
      return unreachable;
    }
  }
}

// ── Panel ────────────────────────────────────────────────────────────────────

export function SplitPanel() {
  const open = useSplitStore((s) => s.open);
  const width = useSplitStore((s) => s.width);
  const wrap = useSplitStore((s) => s.wrap);
  const content = useSplitStore((s) => s.content);
  const closeSplit = useSplitStore((s) => s.closeSplit);
  const setWidth = useSplitStore((s) => s.setWidth);
  const toggleWrap = useSplitStore((s) => s.toggleWrap);

  // Closed → render nothing, so the chat column is full-width (unchanged UX).
  if (!open) return null;

  const textValue = content && "text" in content ? content.text : undefined;

  // Drag-to-resize: the panel is flush to the window's right edge, so its width
  // is simply the distance from the cursor to that edge. During the drag the
  // width is applied imperatively (no React render, no localStorage write per
  // mousemove — persisting would re-serialize content.text 60+×/sec); the store
  // is committed + persisted exactly once on mouseup.
  function startResize(e: React.MouseEvent) {
    e.preventDefault();
    const panel = e.currentTarget.parentElement as HTMLElement | null;
    let latest = width;
    const onMove = (ev: MouseEvent) => {
      latest = clampSplitWidth(window.innerWidth - ev.clientX);
      if (panel) panel.style.width = `${latest}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setWidth(latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  return (
    <aside
      style={{ width }}
      className="relative flex h-full flex-none flex-col border-l bg-card"
    >
      {/* Resize handle straddling the left border. */}
      <div
        onMouseDown={startResize}
        title="Drag to resize"
        className="absolute -left-1 top-0 z-10 h-full w-2 cursor-col-resize hover:bg-primary/20"
      />

      <div className="flex h-12 shrink-0 items-center justify-between gap-2 border-b px-3">
        <span className="min-w-0 truncate text-[13px] font-medium text-foreground">
          {content ? headerTitle(content) : "Split"}
        </span>
        <div className="flex shrink-0 items-center gap-0.5">
          <IconButton
            title={wrap ? "Disable wrap" : "Enable wrap"}
            active={wrap}
            onClick={toggleWrap}
          >
            <WrapText className="size-3.5" />
          </IconButton>
          {textValue !== undefined && <CopyIconButton value={textValue} />}
          <IconButton title="Close (Esc)" onClick={closeSplit}>
            <X className="size-3.5" />
          </IconButton>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {content ? (
          <SplitBody content={content} wrap={wrap} />
        ) : (
          <div className="flex h-full items-center justify-center px-4 text-center text-[13px] text-muted-foreground/70">
            Nothing open. Use “Open in split” on a code block or tool output.
          </div>
        )}
      </div>
    </aside>
  );
}
