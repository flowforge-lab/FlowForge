import { memo, useState, type ReactNode } from "react";
import { isValidElement } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { Check, Copy } from "lucide-react";
import { cn } from "@/lib/utils";

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
        <CopyButton value={raw} />
      </div>
      <pre className="overflow-x-auto px-3 py-2.5 text-[12.5px] leading-relaxed">
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

// Renders assistant Markdown. `react-markdown` escapes raw HTML by default (no
// rehype-raw here) and sanitizes URLs, so model output can't inject markup.
// Memoized on `content` so unrelated re-renders don't re-parse; during streaming
// `content` changes every token, which re-parses — acceptable and smooth for M2.
function MarkdownImpl({ content }: { content: string }) {
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
