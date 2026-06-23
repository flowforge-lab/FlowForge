import type { Attachment } from "@/bindings";

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
      // Tauri v2 webviews expose the original OS path on the `File` object.
      // When absent (mock/browser), fall back to inline base64.
      const path = (file as { path?: string }).path;
      resolve({
        kind: "image",
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
