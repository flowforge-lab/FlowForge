import { useState } from "react";
import { Check, Copy, FileText } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { HighlightedCode, Markdown } from "@/components/markdown";
import { useFilePanelStore } from "@/store/file-panel";
import { Breadcrumb } from "./breadcrumb";
import type { FileContent } from "@/bindings";

// File content viewer (#872): breadcrumb header + copy actions, then the body —
// rendered markdown (toggleable to raw), syntax-highlighted source, a binary
// placeholder, or a truncation notice. Reuses `HighlightedCode`/`Markdown`.

/** Map a file name to a highlight.js language hint. Best-effort — an unknown
 *  extension falls back to no language (plain highlighting). */
function langFromName(name: string): string {
  const ext = name.slice(name.lastIndexOf(".") + 1).toLowerCase();
  const map: Record<string, string> = {
    ts: "typescript",
    tsx: "tsx",
    js: "javascript",
    jsx: "jsx",
    rs: "rust",
    py: "python",
    go: "go",
    rb: "ruby",
    java: "java",
    c: "c",
    h: "c",
    cpp: "cpp",
    cc: "cpp",
    cs: "csharp",
    json: "json",
    toml: "toml",
    yaml: "yaml",
    yml: "yaml",
    sh: "bash",
    bash: "bash",
    html: "html",
    css: "css",
    sql: "sql",
    md: "markdown",
  };
  return map[ext] ?? "";
}

function isMarkdown(name: string): boolean {
  return /\.(md|markdown)$/i.test(name);
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function IconButton({
  onClick,
  title,
  children,
}: {
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className="flex size-6 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
    >
      {children}
    </button>
  );
}

function CopyButton({ value, title }: { value: string; title: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <IconButton
      title={copied ? "Copied" : title}
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

function Body({ path, content }: { path: string; content: FileContent }) {
  const markdownRaw = useFilePanelStore((s) => s.markdownRaw);

  if (content.isBinary) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center text-muted-foreground">
        <FileText className="size-8 opacity-40" />
        <p className="text-[13px]">Binary file ({formatBytes(content.size)})</p>
      </div>
    );
  }

  const text = content.text ?? "";
  const notice = content.truncated ? (
    <p className="border-b bg-amber-500/10 px-4 py-1.5 text-[12px] text-amber-700 dark:text-amber-400">
      Showing the first {formatBytes(byteLength(text))} of{" "}
      {formatBytes(content.size)}.
    </p>
  ) : null;

  if (isMarkdown(path) && !markdownRaw) {
    return (
      <div className="min-h-0 flex-1 overflow-auto">
        {notice}
        <div className="px-4 py-3">
          <Markdown content={text} />
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-0 flex-1 overflow-auto">
      {notice}
      <pre className="min-h-full px-4 py-3 font-mono text-[12.5px] leading-relaxed text-foreground/90">
        <HighlightedCode lang={langFromName(path)} text={text} />
      </pre>
    </div>
  );
}

/** Byte length of a UTF-8 string, for the truncation notice. */
function byteLength(s: string): number {
  return new TextEncoder().encode(s).length;
}

export function FileViewer() {
  const selectedPath = useFilePanelStore((s) => s.selectedPath);
  const content = useFilePanelStore((s) => s.content);
  const loading = useFilePanelStore((s) => s.contentLoading);
  const error = useFilePanelStore((s) => s.contentError);
  const markdownRaw = useFilePanelStore((s) => s.markdownRaw);
  const setMarkdownRaw = useFilePanelStore((s) => s.setMarkdownRaw);
  const toggleExpand = useFilePanelStore((s) => s.toggleExpand);

  if (!selectedPath) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center text-[13px] text-muted-foreground/70">
        Select a file to view its contents.
      </div>
    );
  }

  // Breadcrumb dir clicks reveal the directory in the tree by expanding it.
  const revealDir = (dir: string) => {
    if (dir && !useFilePanelStore.getState().expanded.has(dir)) {
      void toggleExpand(dir);
    }
  };

  return (
    <div className="flex h-full min-w-0 flex-col">
      <div className="flex h-9 shrink-0 items-center justify-between gap-2 border-b px-2">
        <Breadcrumb path={selectedPath} onNavigate={revealDir} />
        <div className="flex shrink-0 items-center gap-0.5">
          {isMarkdown(selectedPath) && content && !content.isBinary && (
            <button
              type="button"
              onClick={() => setMarkdownRaw(!markdownRaw)}
              className={cn(
                "rounded px-1.5 py-0.5 text-[11px] font-medium transition-colors",
                "text-muted-foreground hover:bg-foreground/10 hover:text-foreground",
              )}
            >
              {markdownRaw ? "Rendered" : "Raw"}
            </button>
          )}
          <CopyButton value={selectedPath} title="Copy path" />
          {content?.text != null && (
            <CopyButton value={content.text} title="Copy content" />
          )}
        </div>
      </div>

      {loading && !content ? (
        <div className="flex h-full items-center justify-center text-[13px] text-muted-foreground/70">
          Loading…
        </div>
      ) : error ? (
        <p className="px-4 py-3 text-[12.5px] text-destructive">{error}</p>
      ) : content ? (
        <Body path={selectedPath} content={content} />
      ) : null}
    </div>
  );
}
