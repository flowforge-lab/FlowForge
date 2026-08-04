import {
  createContext,
  memo,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { isValidElement } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { Root } from "hast";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import { Check, Copy, PanelRight } from "@/components/ui/icon";
import { remarkBackslashMath } from "@/lib/remark-backslash-math";
import { splitBlocks } from "@/lib/markdown-blocks";
import { getCachedHighlight, highlight } from "@/lib/shiki";
import { useCopied } from "@/lib/use-copied";
import { openExternalUrl } from "@/lib/about";
import { useSplitStore } from "@/store/split";

// Flatten React children (including the nested <span> tree a highlighter
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

// True while the surrounding message is still streaming. Highlighting is the
// heaviest part of rendering a code block, and while streaming the open block's
// text grows every frame, so highlighting it per frame would be O(n^2) (#104).
// Before #1169 that was enforced by leaving `rehype-highlight` off the
// streaming pipelines; now that highlighting lives in the component rather than
// the rehype pipeline, this context carries the same signal — which also lets
// all three prose instances share one `COMPONENTS` map instead of forking it.
const StreamingContext = createContext(false);

// Syntax-highlighted `<code>` — the single highlighting entry point for the
// whole app. Grammars load lazily (see lib/shiki.ts), so this renders the plain
// text first and swaps in the highlighted tree when the grammar resolves. That
// swap is deliberately a lossless in-place recolour with no spinner or
// skeleton: same text, same font, same metrics, so nothing reflows. A cached
// block (`getCachedHighlight`) skips the plain phase entirely, which is what
// keeps the virtualized transcript (#1143) from re-flashing on every scroll.
function ShikiCodeInner({ code, lang }: { code: string; lang: string }) {
  const [hast, setHast] = useState<Root | null>(() =>
    getCachedHighlight(code, lang),
  );

  useEffect(() => {
    if (hast) return;
    let alive = true;
    void highlight(code, lang).then((result) => {
      if (alive) setHast(result);
    });
    return () => {
      alive = false;
    };
  }, [code, lang, hast]);

  if (!hast) return <code className="shiki">{code}</code>;

  // hast -> React elements rather than dangerouslySetInnerHTML: model output
  // must never become markup (the same reason there is no rehype-raw below),
  // and a real React tree keeps `childrenToText` working for the copy button.
  return toJsxRuntime(hast, { Fragment, jsx, jsxs });
}

// Keyed on the exact code + language so a changed block starts from a fresh
// `useState` seed (a cache hit, when there is one) instead of briefly showing
// the previous block's highlighting. Remounting is cheap precisely because the
// seed comes from the cache.
const ShikiCode = memo(function ShikiCode({
  code,
  lang,
}: {
  code: string;
  lang: string;
}) {
  return <ShikiCodeInner key={`${lang}\0${code}`} code={code} lang={lang} />;
});

// The body of a fenced block inside a message: highlighted once the turn has
// settled, plain while it is still streaming.
function CodeBody({ code, lang }: { code: string; lang: string }) {
  const streaming = useContext(StreamingContext);
  if (streaming) return <code className="shiki">{code}</code>;
  return <ShikiCode code={code} lang={lang} />;
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
// `children` is the <code> subtree; `raw` is its plain text.
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

    const language = match?.[1] ?? "text";
    const raw = text.replace(/\n$/, "");

    return (
      <CodeBlock language={language} raw={raw}>
        <CodeBody code={raw} lang={language} />
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

// One markdown block, rendered through the same pipeline as every other prose
// instance. Memoized so a closed block — whose text never changes again once
// closed — is parsed exactly once, no matter how many more frames the
// surrounding message keeps streaming.
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
// Syntax highlighting is the heaviest part of rendering a message. While
// `streaming`, `content` grows by a token each render, so highlighting every
// frame is O(n^2) and stalls the UI thread on long replies (#104). We suppress
// it during streaming — markdown structure still renders live — and highlight
// once the turn finishes (`streaming` flips to false). Since #1169 that
// suppression is carried by `StreamingContext` rather than by omitting a rehype
// plugin, so all three prose instances below now run the *same* plugin list and
// can no longer drift into rendering the same message two different ways.
//
// On top of that, while streaming we split `content` into closed blocks (won't
// change again) and one open tail block (still growing) (#844). Each closed
// block renders through its own memoized `MarkdownBlock`, so React skips
// re-parsing it on every subsequent frame — only the small open tail gets
// parsed each frame, bounding per-frame cost to roughly the last block's size
// instead of the whole message. Once the turn finishes, `streaming` flips to
// false and the full content re-parses once, unsplit, with highlighting.
//
// KaTeX, unlike highlighting, does run while streaming (#1102): it is cheap,
// only touches the math nodes, and leaving it off would render formulas as code
// blocks mid-stream and then swap them for typeset math when the turn settles —
// exactly the inconsistency #844's equivalence test guards.
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
      <StreamingContext.Provider value={true}>
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
      </StreamingContext.Provider>
    );
  }

  return (
    <div className="ff-prose">
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        rehypePlugins={KATEX_PLUGINS}
        components={COMPONENTS}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

export const Markdown = memo(MarkdownImpl);

// Syntax-highlights a raw code string. Returns the bare highlighted <code> (no
// <pre>) so callers control wrapping/scroll — used by the split panel, notebook
// cell output, and the file viewer. Before #1169 this wrapped `text` in a
// synthetic markdown fence and ran it back through react-markdown just to reach
// the highlighter; now it hands the string straight to Shiki, so no fence
// escaping is needed and the text can never be re-interpreted as markdown.
function HighlightedCodeImpl({ lang, text }: { lang: string; text: string }) {
  return <ShikiCode code={text.replace(/\n$/, "")} lang={lang} />;
}

export const HighlightedCode = memo(HighlightedCodeImpl);
