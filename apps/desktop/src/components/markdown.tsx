import { memo, useState, type ReactNode } from "react";
import { isValidElement } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { Check, Copy, PanelRight } from "@/components/ui/icon";
import { splitBlocks } from "@/lib/markdown-blocks";
import { cn } from "@/lib/utils";
import { useSplitStore } from "@/store/split";

// Flatten React children (including the nested <span> tree rehype-highlight
// produces) back into plain text — used as the source for the copy button.
function childrenToText(children: ReactNode): string {
  if (children == null || typeof children === "boolean") return "";
  if (typeof children === "string" || typeof children === "number") {
    return String(children);
  }
  if (Array.isArray(children)) return children.map(childrenToText).join("");
  if (isValidElement(children)) {
    return childrenToText(
      (children.props as { children?: ReactNode }).children,
    );
  }
  return "";
}

function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard can be unavailable (permissions / insecure context); fail quiet.
    }
  }

  return (
    <button
      type="button"
      onClick={copy}
      title={copied ? "Copied" : "Copy code"}
      className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground"
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

// Pops the block into the right-hand split panel (Issue #11).
function OpenInSplitButton({
  language,
  raw,
}: {
  language: string;
  raw: string;
}) {
  const openInSplit = useSplitStore((s) => s.openInSplit);
  return (
    <button
      type="button"
      onClick={() => openInSplit({ kind: "code", lang: language, text: raw })}
      title="Open in split"
      className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground/80 transition-colors hover:bg-foreground/10 hover:text-foreground"
    >
      <PanelRight className="size-3" />
      Split
    </button>
  );
}

// A fenced code block: language label + copy button over the highlighted code.
// `children` is the already-highlighted <code> subtree; `raw` is its plain text.
function CodeBlock({
  language,
  raw,
  children,
}: {
  language: string;
  raw: string;
  children: ReactNode;
}) {
  return (
    <div className="ff-code my-2 overflow-hidden rounded-md border">
      <div className="ff-code__bar flex items-center justify-between px-3 py-1">
        <span className="font-mono text-[11px] uppercase tracking-wide text-muted-foreground/70">
          {language}
        </span>
        <div className="flex items-center gap-1">
          <OpenInSplitButton language={language} raw={raw} />
          <CopyButton value={raw} />
        </div>
      </div>
      <pre className="overflow-x-auto whitespace-pre-wrap break-words px-3 py-2.5 text-[13.5px] leading-relaxed">
        {children}
      </pre>
    </div>
  );
}

// react-markdown component overrides. `pre` is flattened so each fenced block's
// chrome comes solely from CodeBlock (avoids an extra wrapping <pre>).
const COMPONENTS = {
  pre: ({ children }: { children?: ReactNode }) => <>{children}</>,
  code: ({
    className,
    children,
    ...props
  }: {
    className?: string;
    children?: ReactNode;
  }) => {
    const text = childrenToText(children);
    const match = /language-(\w+)/.exec(className ?? "");
    // Fenced blocks always have a language class or span multiple lines; bare
    // single-line spans are inline code.
    const isBlock = match !== null || text.includes("\n");

    if (!isBlock) {
      return (
        <code className="ff-inline-code" {...props}>
          {children}
        </code>
      );
    }

    return (
      <CodeBlock language={match?.[1] ?? "text"} raw={text.replace(/\n$/, "")}>
        <code className={cn("hljs", className)} {...props}>
          {children}
        </code>
      </CodeBlock>
    );
  },
  // Links open in the OS browser; never same-window navigate the Tauri shell.
  a: ({ children, ...props }: { children?: ReactNode; href?: string }) => (
    <a target="_blank" rel="noreferrer noopener" {...props}>
      {children}
    </a>
  ),
};

// One markdown block, rendered through the same (highlight-free) pipeline as
// the streaming path. Memoized so a closed block — whose text never changes
// again once closed — is parsed exactly once, no matter how many more frames
// the surrounding message keeps streaming.
const MarkdownBlock = memo(function MarkdownBlock({ text }: { text: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>
      {text}
    </ReactMarkdown>
  );
});

// Renders assistant Markdown. `react-markdown` escapes raw HTML by default (no
// rehype-raw here) and sanitizes URLs, so model output can't inject markup.
//
// `rehype-highlight` (highlight.js) is the heaviest part of the pipeline and runs
// over the whole document on every render. While `streaming`, `content` grows by a
// token each render, so highlighting there is O(n^2) and stalls the UI thread on
// long replies (#104). We drop the highlight pass during streaming — markdown
// structure still renders live — and run the full pipeline once when the turn
// finishes (`streaming` flips to false), which highlights the final text.
//
// On top of that, while streaming we split `content` into closed blocks (won't
// change again) and one open tail block (still growing) (#844). Each closed
// block renders through its own memoized `MarkdownBlock`, so React skips
// re-parsing it on every subsequent frame — only the small open tail gets
// parsed each frame, bounding per-frame cost to roughly the last block's size
// instead of the whole message. Once the turn finishes, `streaming` flips to
// false and the full content re-parses once, unsplit, with highlighting.
function MarkdownImpl({
  content,
  streaming = false,
}: {
  content: string;
  streaming?: boolean;
}) {
  if (streaming) {
    const { closed, open } = splitBlocks(content);
    return (
      <div className="ff-prose">
        {closed.map((block, i) => (
          <MarkdownBlock key={i} text={block} />
        ))}
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>
          {open}
        </ReactMarkdown>
      </div>
    );
  }

  return (
    <div className="ff-prose">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={COMPONENTS}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

export const Markdown = memo(MarkdownImpl);

// Smallest backtick fence that can safely wrap `text` — longer than any run of
// backticks inside it, so code containing ``` won't break out of the block.
function safeFence(text: string): string {
  const longest = (text.match(/`+/g) ?? []).reduce(
    (m, run) => Math.max(m, run.length),
    0,
  );
  return "`".repeat(Math.max(3, longest + 1));
}

const HIGHLIGHT_COMPONENTS = {
  // Flattened: the caller supplies its own <pre>, so we emit just the <code>.
  pre: ({ children }: { children?: ReactNode }) => <>{children}</>,
  code: ({
    className,
    children,
    ...props
  }: {
    className?: string;
    children?: ReactNode;
  }) => (
    <code className={cn("hljs", className)} {...props}>
      {children}
    </code>
  ),
};

// Syntax-highlights a raw code string, reusing #7's rehype-highlight pipeline
// and the shared `.hljs` theme in index.css. Returns the bare highlighted
// <code> (no <pre>) so callers control wrapping/scroll. Used by the split panel.
function HighlightedCodeImpl({ lang, text }: { lang: string; text: string }) {
  const fence = safeFence(text);
  return (
    <ReactMarkdown
      rehypePlugins={[rehypeHighlight]}
      components={HIGHLIGHT_COMPONENTS}
    >
      {`${fence}${lang}\n${text}\n${fence}`}
    </ReactMarkdown>
  );
}

export const HighlightedCode = memo(HighlightedCodeImpl);
