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
]);

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
  const ext = file.name.split(".").pop()?.toLowerCase();
  if (ext && DOCUMENT_EXTENSIONS.has(ext)) {
    return "document";
  }
  return null;
}

// Shared helper for #339: reads a `File` and produces an `Attachment` object
// matching the #332 multimodal data model. Prefers the OS file-path as the
// `source` when available (Tauri desktop exposes `File.path`), falling back to
// inline base64 in the mock/browser where no path exists.
export async function fileToAttachment(file: File): Promise<Attachment> {
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
      // `canStage` rejects unsupported types before we get here, so a null
      // kind is a real bug rather than a user-facing case -- surface it as an
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
