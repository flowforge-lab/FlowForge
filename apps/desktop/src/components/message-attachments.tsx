import { useEffect, useState } from "react";
import { Dialog } from "radix-ui";
import { FileText, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { formatBytes } from "@/lib/memory-view";
import type { Attachment, AttachmentSource } from "@/bindings";

// Read-only render of a sent message's attachments in the transcript (#341):
// image thumbnails (click to enlarge) and document chips. The composer renders
// its own removable variant pre-send (#340); these are already sent, so there is
// no remove affordance here.

// Short, human type label from the IANA media type: "image/png" -> "PNG".
function typeLabel(mediaType: string, kind: Attachment["kind"]): string {
  return mediaType.split("/")[1]?.toUpperCase() ?? kind.toUpperCase();
}

// Inline (base64) previews resolve synchronously; a path reference is resolved
// lazily through Tauri's asset protocol (dynamic import, like ipc.ts, so the mock
// build never statically bundles Tauri). Returns undefined until/unless resolved.
function useAttachmentSrc(
  source: AttachmentSource,
  mediaType: string,
  enabled: boolean,
): string | undefined {
  const [src, setSrc] = useState<string | undefined>(() =>
    enabled && source.type === "inline"
      ? `data:${mediaType};base64,${source.value}`
      : undefined,
  );
  useEffect(() => {
    if (!enabled || source.type !== "path") return;
    let alive = true;
    void import("@tauri-apps/api/core")
      .then(({ convertFileSrc }) => {
        if (alive) setSrc(convertFileSrc(source.value));
      })
      .catch(() => {
        /* no asset protocol (e.g. mock/browser) — keep the icon fallback */
      });
    return () => {
      alive = false;
    };
  }, [enabled, source.type, source.value, mediaType]);
  return src;
}

function ImageThumb({
  attachment,
  onOpen,
}: {
  attachment: Attachment;
  onOpen: (src: string) => void;
}) {
  const { mediaType, source, name, bytes, kind } = attachment;
  const src = useAttachmentSrc(source, mediaType, true);
  const label = name ?? typeLabel(mediaType, kind);

  if (!src) {
    // No preview available (e.g. a path with no asset protocol): fall back to the
    // same chip presentation a document gets, so the attachment is still visible.
    return <DocChip attachment={attachment} />;
  }
  return (
    <button
      type="button"
      onClick={() => onOpen(src)}
      title={`${label} · ${typeLabel(mediaType, kind)} · ${formatBytes(bytes)}`}
      aria-label={`Open ${label}`}
      className="size-16 shrink-0 overflow-hidden rounded-md border bg-background transition-shadow hover:shadow-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <img src={src} alt={label} className="size-full object-cover" />
    </button>
  );
}

function DocChip({ attachment }: { attachment: Attachment }) {
  const { mediaType, name, bytes, kind } = attachment;
  const type = typeLabel(mediaType, kind);
  const label = name ?? type;
  return (
    <div
      className="flex items-center gap-2 rounded-md border bg-muted/40 py-1 pl-1 pr-2"
      title={name ? `${name} · ${type} · ${formatBytes(bytes)}` : undefined}
    >
      <div className="flex size-9 shrink-0 items-center justify-center rounded bg-background text-muted-foreground">
        <FileText className="size-4" />
      </div>
      <div className="flex min-w-0 flex-col">
        <span className="max-w-32 truncate text-xs text-foreground">
          {label}
        </span>
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
          {type} · {formatBytes(bytes)}
        </span>
      </div>
    </div>
  );
}

export function MessageAttachments({
  attachments,
}: {
  attachments: Attachment[];
}) {
  // The full-size image shown in the lightbox; null when closed.
  const [preview, setPreview] = useState<string | null>(null);

  return (
    <>
      <div className="flex flex-wrap justify-end gap-1.5">
        {attachments.map((att, idx) =>
          att.kind === "image" ? (
            <ImageThumb key={idx} attachment={att} onOpen={setPreview} />
          ) : (
            <DocChip key={idx} attachment={att} />
          ),
        )}
      </div>

      <Dialog.Root
        open={preview !== null}
        onOpenChange={(open) => !open && setPreview(null)}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-50 bg-black/70 data-open:animate-in data-open:fade-in-0 data-closed:animate-out data-closed:fade-out-0" />
          <Dialog.Content
            className={cn(
              "fixed top-1/2 left-1/2 z-50 -translate-x-1/2 -translate-y-1/2",
              "data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            )}
          >
            {/* Radix requires an accessible title; visually hidden for an image preview. */}
            <Dialog.Title className="sr-only">Attachment preview</Dialog.Title>
            {preview && (
              <img
                src={preview}
                alt="Attachment preview"
                className="max-h-[85vh] max-w-[85vw] rounded-lg object-contain shadow-2xl"
              />
            )}
            <Dialog.Close asChild>
              <Button
                variant="outline"
                size="icon"
                className="absolute -right-3 -top-3 size-8 rounded-full shadow-md"
                aria-label="Close preview"
              >
                <X className="size-4" />
              </Button>
            </Dialog.Close>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
