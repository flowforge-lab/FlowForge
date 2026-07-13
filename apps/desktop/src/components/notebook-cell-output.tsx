import { useEffect, useState } from "react";
import { Check, Loader2, X } from "@/components/ui/icon";
import { Badge } from "@/components/ui/badge";
import { HighlightedCode } from "@/components/markdown";
import {
  parseNotebookStep,
  parseKernelStatus,
  type NotebookImageRef,
  type NotebookStep,
  type NotebookVariable,
} from "@/lib/notebook-output";
import { cn } from "@/lib/utils";
import type { ToolStep } from "@/store/chat";

/**
 * Notebook-styled cell renderer for the `notebook_runner` tool (#871 FE-2,
 * #879 for the Phase 3 images/variables blocks).
 *
 * Lays out a tool step as a notebook cell: an optional `code` block (for
 * `run_cell`), the textual output stream, an ok/error badge, and — when the
 * backend's `FF_NB_META` trailer carried them — figures and a variables
 * table. The `status` action gets a small kernel-state pill derived from the
 * canonical `kernel <id> — <state>; pid=…; cells executed=…` line the
 * backend emits.
 */
export function NotebookCellOutput({ step }: { step: ToolStep }) {
  const parsed = parseNotebookStep(
    step.tool,
    step.args,
    step.result,
    step.status === "error"
      ? "error"
      : step.status === "done"
        ? "done"
        : "running",
  );
  if (!parsed) return null;
  return (
    <NotebookCellBody
      step={parsed}
      live={step.output}
      running={step.status === "running"}
    />
  );
}

function NotebookCellBody({
  step,
  live,
  running,
}: {
  step: NotebookStep;
  /** Live `tool:output` stream accumulated while the cell runs. */
  live: string | undefined;
  running: boolean;
}) {
  // While running we mirror the live stream so slow cells visibly progress
  // (#680) — same convention as the generic OutputBlock. On completion
  // `step.result` (the parsed `output` above) supersedes it.
  const textWhileRunning =
    running && live !== undefined && live.length > 0 ? live : null;

  return (
    <div className="space-y-2">
      {step.code !== null && (
        <NotebookSection label="code">
          <HighlightedCode lang="python" text={step.code} />
        </NotebookSection>
      )}

      {step.action === "status" ? (
        <StatusLine raw={textWhileRunning ?? step.output} />
      ) : (
        <NotebookSection
          label={step.action === "run_cell" ? "output" : "result"}
          error={step.errored}
          trailing={
            step.action === "run_cell" ? (
              <CellStatusBadge
                errored={step.errored}
                running={running && textWhileRunning !== null}
                truncated={step.parsedExceptionTrailer}
              />
            ) : null
          }
        >
          {textWhileRunning !== null ? textWhileRunning : step.output}
        </NotebookSection>
      )}

      {step.images && step.images.length > 0 ? (
        <NotebookImages images={step.images} />
      ) : null}

      {step.variables && step.variables.length > 0 ? (
        <NotebookVariablesTable variables={step.variables} />
      ) : null}
    </div>
  );
}

// Convert raw bytes to a base64 string. Chunked so we don't build the binary
// string one char at a time (immutable-string concat is O(n²) and stutters on
// a 500KB+ figure); each chunk is small enough to spread into
// `String.fromCharCode` without blowing the argument-count limit.
function bytesToBase64(bytes: Uint8Array): string {
  const CHUNK = 8192;
  const parts: string[] = [];
  for (let i = 0; i < bytes.length; i += CHUNK) {
    parts.push(String.fromCharCode(...bytes.subarray(i, i + CHUNK)));
  }
  return btoa(parts.join(""));
}

/** Read one image file's bytes and build a `data:` URI. `null` on any
 *  failure (no Tauri runtime, file gone, read error) — the caller skips it. */
async function readImageAsDataUrl(
  img: NotebookImageRef,
): Promise<string | null> {
  try {
    const { readFile } = await import("@tauri-apps/plugin-fs");
    const bytes = await readFile(img.path);
    return `data:${img.mediaType};base64,${bytesToBase64(bytes)}`;
  } catch {
    return null;
  }
}

/** Resolves each image path to a `data:` URI, keyed by path. Depends on the
 *  path list (not the `images` array reference, which is a fresh array every
 *  parse) so it only re-reads when the underlying step actually changes. */
function useResolvedImages(images: NotebookImageRef[]): Record<string, string> {
  const key = images.map((img) => `${img.path}|${img.mediaType}`).join("\n");
  const [dataUrls, setDataUrls] = useState<Record<string, string>>({});

  useEffect(() => {
    if (images.length === 0) return;
    let alive = true;
    void Promise.all(
      images.map(
        async (img) => [img.path, await readImageAsDataUrl(img)] as const,
      ),
    ).then((results) => {
      if (!alive) return;
      const next: Record<string, string> = {};
      for (const [path, dataUrl] of results) {
        if (dataUrl) next[path] = dataUrl;
      }
      setDataUrls(next);
    });
    return () => {
      alive = false;
    };
    // `key` is the real dependency (see doc comment above); `images` itself
    // changes identity every render even when its contents don't.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return dataUrls;
}

function NotebookImages({ images }: { images: NotebookImageRef[] }) {
  const dataUrls = useResolvedImages(images);
  const loaded = images.filter((img) => dataUrls[img.path]);
  if (loaded.length === 0) return null;
  return (
    <div>
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
        figures
      </span>
      <div className="mt-1 flex flex-wrap gap-2">
        {loaded.map((img) => (
          <img
            key={img.path}
            src={dataUrls[img.path]}
            alt="Cell figure"
            className="max-h-64 max-w-full rounded border border-border object-contain"
          />
        ))}
      </div>
    </div>
  );
}

function NotebookVariablesTable({
  variables,
}: {
  variables: NotebookVariable[];
}) {
  return (
    <div>
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
        variables
      </span>
      <table className="mt-1 w-full border-collapse text-[11px]">
        <thead>
          <tr className="text-left text-muted-foreground/70">
            <th className="py-0.5 pr-2 font-medium">name</th>
            <th className="py-0.5 pr-2 font-medium">type</th>
            <th className="py-0.5 font-medium">repr</th>
          </tr>
        </thead>
        <tbody>
          {variables.map((v) => (
            <tr key={v.name} className="border-t border-border">
              <td className="py-0.5 pr-2 font-mono text-foreground">
                {v.name}
              </td>
              <td className="py-0.5 pr-2 text-muted-foreground">
                {v.type ?? "—"}
              </td>
              <td
                className="max-w-0 truncate py-0.5 font-mono text-foreground/80"
                title={v.repr}
              >
                {v.repr}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function NotebookSection({
  label,
  error,
  trailing,
  children,
}: {
  label: string;
  error?: boolean;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-0.5 flex items-center justify-between gap-2">
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
          {label}
        </span>
        {trailing}
      </div>
      <pre
        className={cn(
          "max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed",
          error ? "text-destructive" : "text-foreground/90",
        )}
      >
        {children}
      </pre>
    </div>
  );
}

function CellStatusBadge({
  errored,
  running,
  truncated,
}: {
  errored: boolean;
  running: boolean;
  truncated: boolean;
}) {
  if (running) {
    return (
      <Badge tone="sky">
        <Loader2 className="mr-1 size-3 animate-spin" />
        running
      </Badge>
    );
  }
  if (errored) {
    return (
      <Badge tone="destructive">
        <X className="mr-1 size-3" />
        {truncated ? "exception" : "error"}
      </Badge>
    );
  }
  return (
    <Badge tone="emerald">
      <Check className="mr-1 size-3" />
      ok
    </Badge>
  );
}

function StatusLine({ raw }: { raw: string }) {
  const status = parseKernelStatus(raw);
  // No kernel: a quiet dot + label, no error copy. The StatusIcon in the
  // step header already shows a green check on a successful `status` call,
  // so we keep this minimal to avoid duplicating the affordance.
  if (status.state === "no-kernel") {
    return (
      <p className="text-[11px] text-muted-foreground/70">
        no kernel for this session
      </p>
    );
  }
  // Unknown / unparseable line: fall back to the raw text so the user can
  // always read what the backend reported.
  if (status.state === "unknown") {
    return (
      <p className="font-mono text-[11px] leading-relaxed text-foreground/80">
        {raw.trim() || "(no output)"}
      </p>
    );
  }
  const dotTone = status.state === "live" ? "bg-emerald-500" : "bg-destructive";
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
      <span className={cn("size-1.5 shrink-0 rounded-full", dotTone)} />
      <span className="font-medium text-foreground">
        {status.state === "live" ? "kernel running" : "kernel dead"}
      </span>
      {status.kernelId && (
        <span className="font-mono text-muted-foreground/70">
          {status.kernelId}
        </span>
      )}
      {status.executionCount !== null && (
        <span className="text-muted-foreground/70">
          {status.executionCount} cell{status.executionCount === 1 ? "" : "s"}{" "}
          executed
        </span>
      )}
    </div>
  );
}
