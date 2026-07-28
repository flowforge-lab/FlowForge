import { memo, type ReactNode } from "react";
import { isValidElement } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import { Check, Copy, PanelRight } from "@/components/ui/icon";
import { remarkBackslashMath } from "@/lib/remark-backslash-math";
import { splitBlocks } from "@/lib/markdown-blocks";
import { cn } from "@/lib/utils";
import { useCopied } from "@/lib/use-copied";
import { openExternalUrl } from "@/lib/about";
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
  const { copied, copy } = useCopied();

  return (
    <button
      type="button"
      onClick={() => void copy(value)}
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
  // Links open in the OS browser — never same-window navigate the Tauri shell.
  // In packaged Tauri the previous `target="_blank"` was a no-op (there are no
  // tabs) and clicks stayed inside the webview; here http(s)/mailto clicks route
  // through `openExternalUrl` so they hit the system browser (#1129). Relative
  // and anchor links fall through to natural browser navigation. The scheme
  // allowlist also keeps untrusted LLM-supplied URLs (e.g. `javascript:`) from
  // ever reaching `openExternalUrl`.
  a: ({ children, ...props }: { children?: ReactNode; href?: string }) => {
    const href = typeof props.href === "string" ? props.href : "";
    const isExternal = /^(https?:|mailto:)/i.test(href);

    if (!isExternal) {
      return (
        <a rel="noreferrer noopener" {...props}>
          {children}
        </a>
      );
    }

    return (
      <a
        rel="noreferrer noopener"
        {...props}
        onClick={(e) => {
          e.preventDefault();
          void openExternalUrl(href);
        }}
      >
        {children}
      </a>
    );
  },
};

// Math support (#1102). `remarkMath` handles `$…$`/`$$…$$`; `remarkBackslashMath`
// handles the `\(…\)`/`\[…\]` forms OpenAI-family models emit. Both lower to
// `<code class="language-math …">`, which `COMPONENTS.code` would otherwise
// render as a code block complete with copy/split chrome — so `rehypeKatex`
// (which splices those elements out entirely) must accompany them on every
// instance, streaming included. Shared consts so the three prose instances
// below cannot drift apart and render the same message two different ways.
const REMARK_PLUGINS = [remarkGfm, remarkBackslashMath, remarkMath];
// A malformed formula renders as inline `.katex-error` text instead of throwing
// and blanking the whole message.
const KATEX: [typeof rehypeKatex, { throwOnError: boolean }] = [
  rehypeKatex,
  { throwOnError: false },
];
const KATEX_PLUGINS = [KATEX];

// One markdown block, rendered through the same (highlight-free) pipeline as
// the streaming path. Memoized so a closed block — whose text never changes
// again once closed — is parsed exactly once, no matter how many more frames
// the surrounding message keeps streaming.
const MarkdownBlock = memo(function MarkdownBlock({ text }: { text: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={REMARK_PLUGINS}
      rehypePlugins={KATEX_PLUGINS}
      components={COMPONENTS}
    >
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
//
// KaTeX, unlike highlighting, does run while streaming (#1102): it is cheap next
// to highlight.js, only touches the math nodes, and leaving it off would render
// formulas as code blocks mid-stream and then swap them for typeset math when
// the turn settles — exactly the inconsistency #844's equivalence test guards.
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
        <ReactMarkdown
          remarkPlugins={REMARK_PLUGINS}
          rehypePlugins={KATEX_PLUGINS}
          components={COMPONENTS}
        >
          {open}
        </ReactMarkdown>
      </div>
    );
  }

  return (
    <div className="ff-prose">
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        rehypePlugins={[...KATEX_PLUGINS, rehypeHighlight]}
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
// No remark plugins here: the document is nothing but one fence, so gfm and the
// math plugins would have nothing outside it to act on.
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
