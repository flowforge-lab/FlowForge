import type { Attachment, AttachmentKind } from "@/bindings";

// Document media types FlowForge can attach (#504). Mirrors the Bedrock
// `DocumentFormat` allowlist; JSON is included and routed to Txt backend-side.
const DOCUMENT_MEDIA_TYPES = new Set<string>([
  "text/csv",
  "application/msword",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "text/html",
  "text/markdown",
  "application/pdf",
  "text/plain",
  "application/vnd.ms-excel",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/json",
  "text/json",
  // Python source (#842). Browsers report `text/x-python` (or nothing) for
  // `.py`; empty MIME falls through to the extension check below.
  "text/x-python",
  "application/x-python-code",
]);

const DOCUMENT_EXTENSIONS = new Set<string>([
  "csv",
  "doc",
  "docx",
  "html",
  "htm",
  "md",
  "markdown",
  "pdf",
  "txt",
  "xls",
  "xlsx",
  "json",
  // Source-code attachments (#842). `.py` goes to the model as text; `.ipynb`
  // is converted to readable text in `fileToAttachment` (raw notebook JSON
  // wastes context), then staged as `text/plain`.
  "py",
  "ipynb",
]);

/** Lower-cased file extension without the dot, or `undefined` if none. */
function extensionOf(name: string): string | undefined {
  return name.includes(".") ? name.split(".").pop()?.toLowerCase() : undefined;
}

// Classify a file as an image or document attachment (#504). Images key off the
// `image/` MIME prefix; everything in the document allowlist (by media type or
// file extension) becomes a `document`. Returns `null` for anything else so
// callers can reject unsupported files rather than mislabel them.
export function attachmentKindFor(file: File): AttachmentKind | null {
  if (file.type.startsWith("image/")) {
    return "image";
  }
  if (DOCUMENT_MEDIA_TYPES.has(file.type.toLowerCase())) {
    return "document";
  }
  const ext = extensionOf(file.name);
  if (ext && DOCUMENT_EXTENSIONS.has(ext)) {
    return "document";
  }
  return null;
}

/** True when the file is a Jupyter notebook (converted to text before send). */
export function isNotebook(file: File): boolean {
  return extensionOf(file.name) === "ipynb";
}

// Convert a Jupyter notebook (`.ipynb`) to readable plain text (#842). A raw
// notebook is JSON with cells + metadata + (often base64) outputs; sending that
// verbatim wastes model context, so we extract only the source: markdown/raw
// cells as-is, code cells fenced with the kernel language. Cell `outputs`
// (execution counts, stdout, base64 images) are dropped. Tolerant of malformed
// input: on a parse failure or unexpected shape, returns the original text so the
// user still gets something rather than an error.
export function ipynbToText(raw: string): string {
  let nb: unknown;
  try {
    nb = JSON.parse(raw);
  } catch {
    return raw;
  }
  const cells = (nb as { cells?: unknown }).cells;
  if (!Array.isArray(cells)) return raw;

  const lang =
    (nb as { metadata?: { kernelspec?: { language?: string } } }).metadata
      ?.kernelspec?.language ?? "python";

  const blocks: string[] = [];
  for (const cell of cells) {
    if (typeof cell !== "object" || cell === null) continue;
    const c = cell as { cell_type?: string; source?: unknown };
    // `source` is either a string or an array of line strings (nbformat).
    const src = Array.isArray(c.source)
      ? c.source.join("")
      : typeof c.source === "string"
        ? c.source
        : "";
    const text = src.replace(/\s+$/, "");
    if (text.length === 0) continue;
    if (c.cell_type === "code") {
      blocks.push("```" + lang + "\n" + text + "\n```");
    } else {
      blocks.push(text);
    }
  }
  return blocks.join("\n\n");
}

// Why a dropped/pasted/picked file could not be staged (#723). `unsupported` =
// not an image or a known document type; `vision-gated`/`doc-gated` = a recognized
// kind the resolved model can't accept (the composer's capability gate, #504).
// Deliberately distinct from `AttachmentKind` values so a stageable "document"
// is never mistaken for a rejection.
export type RejectionReason = "unsupported" | "vision-gated" | "doc-gated";

/** The per-model capability gate the composer already computes (#504). */
export interface AttachGate {
  visionGated: boolean;
  docGated: boolean;
}

// Decide a file's fate against the model gate: its `AttachmentKind` when it can
// be staged, or a `RejectionReason` when it can't. Reuses `attachmentKindFor` so
// the classification stays in one place; callers surface the reason to the user
// instead of silently dropping the file.
export function classifyForStaging(
  file: File,
  gate: AttachGate,
): AttachmentKind | RejectionReason {
  const kind = attachmentKindFor(file);
  if (kind === null) return "unsupported";
  if (kind === "image" && gate.visionGated) return "vision-gated";
  if (kind === "document" && gate.docGated) return "doc-gated";
  return kind;
}

// A short, human notice summarizing why some files weren't staged (#723). Groups
// by reason so a mixed batch reads clearly, e.g. "Skipped 2 files: unsupported
// type" or "This model can't accept images; skipped 1 file: unsupported type".
export function describeRejections(reasons: RejectionReason[]): string | null {
  if (reasons.length === 0) return null;
  const parts: string[] = [];
  // Collapse the both-gated case so "this model" doesn't repeat.
  const vision = reasons.includes("vision-gated");
  const doc = reasons.includes("doc-gated");
  if (vision && doc) {
    parts.push("this model can't accept images or documents");
  } else if (vision) {
    parts.push("this model can't accept images");
  } else if (doc) {
    parts.push("this model can't accept documents");
  }
  const unsupported = reasons.filter((r) => r === "unsupported").length;
  if (unsupported > 0) {
    parts.push(
      `skipped ${unsupported} ${unsupported === 1 ? "file" : "files"}: unsupported type`,
    );
  }
  // Sentence-case the first clause only; join the rest with "; ".
  const joined = parts.join("; ");
  return joined.charAt(0).toUpperCase() + joined.slice(1);
}

// Shared helper for #339: reads a `File` and produces an `Attachment` object
// matching the #332 multimodal data model. Prefers the OS file-path as the
// `source` when available (Tauri desktop exposes `File.path`), falling back to
// inline base64 in the mock/browser where no path exists.
export async function fileToAttachment(file: File): Promise<Attachment> {
  // A `.ipynb` is converted to readable text before send (#842): read as text,
  // extract cells, and stage as an INLINE `text/plain` document. The OS path is
  // unusable here — the file on disk is still raw notebook JSON — so the converted
  // text is the payload, base64-encoded to match the inline `Attachment` shape.
  // Uses FileReader (not `File.text()`) for parity with the base64 path below and
  // broad environment support.
  if (isNotebook(file)) {
    const raw = await readAsText(file);
    const converted = ipynbToText(raw);
    return {
      kind: "document",
      mediaType: "text/plain",
      source: { type: "inline", value: base64Utf8(converted) },
      name: file.name || undefined,
      bytes: new TextEncoder().encode(converted).length,
    };
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== "string") {
        reject(new Error("expected data URL"));
        return;
      }
      const base64 = reader.result.split(",")[1];
      if (!base64) {
        reject(new Error("no base64 payload"));
        return;
      }
      // `classifyForStaging` rejects unsupported types before we get here, so a
      // null kind is a real bug rather than a user-facing case -- surface it as an
      // error instead of silently mislabeling the file as an image.
      const kind = attachmentKindFor(file);
      if (!kind) {
        reject(
          new Error(`unsupported attachment type: ${file.type || file.name}`),
        );
        return;
      }
      // Tauri v2 webviews expose the original OS path on the `File` object.
      // When absent (mock/browser), fall back to inline base64.
      const path = (file as { path?: string }).path;
      resolve({
        kind,
        mediaType: file.type,
        source: path
          ? { type: "path", value: path }
          : { type: "inline", value: base64 },
        name: file.name || undefined,
        bytes: file.size,
      });
    };
    reader.onerror = () =>
      reject(reader.error ?? new Error("failed to read file"));
    reader.readAsDataURL(file);
  });
}

// Base64-encode a UTF-8 string. `btoa` operates on Latin-1, so a notebook with
// non-ASCII content (comments, output, emoji) would throw or corrupt; round-trip
// through UTF-8 bytes first. Produces the same base64-of-raw-bytes the backend
// decodes for an inline `Attachment.source.value`.
function base64Utf8(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

// Read a File's contents as text via FileReader (works in the desktop webview and
// jsdom, unlike `File.text()` in some environments).
function readAsText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () =>
      resolve(typeof reader.result === "string" ? reader.result : "");
    reader.onerror = () =>
      reject(reader.error ?? new Error("failed to read file"));
    reader.readAsText(file);
  });
}
