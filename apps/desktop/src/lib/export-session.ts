// Session export (#278). A thin client over the backend `export_session` command:
// it asks the backend for the serialized string (lossless JSON or Markdown), then
// writes it to a user-chosen path. Inside Tauri that's a native save dialog +
// `plugin-fs` write; in a plain browser (mock/dev) it falls back to a Blob
// download so the flow is exercisable under `VITE_FF_MOCK=1`.

import { ipc } from "@/lib/ipc";
import { saveTextToFile, type SaveResult } from "@/lib/save-file";
import type { Format } from "@/bindings/Format";

const EXT: Record<Format, string> = { markdown: "md", json: "json" };
const MIME: Record<Format, string> = {
  markdown: "text/markdown",
  json: "application/json",
};
const FILTER_NAME: Record<Format, string> = {
  markdown: "Markdown",
  json: "JSON",
};

/** A safe download filename from a session title: `My chat` → `my-chat.md`. */
export function exportFilename(
  title: string | null | undefined,
  format: Format,
): string {
  const stem =
    (title ?? "")
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "session";
  return `${stem}.${EXT[format]}`;
}

export type ExportResult = SaveResult;

/**
 * Export `sessionId` as `format`, writing to a user-chosen file via the shared
 * {@link saveTextToFile} core. The backend serialize is lazy — it runs only after the
 * save dialog is confirmed, so a cancelled dialog makes no `export_session` call.
 * Throws if the backend export or the write fails — the caller surfaces an error toast.
 */
export async function exportSessionToFile(
  sessionId: string,
  title: string | null | undefined,
  format: Format,
): Promise<ExportResult> {
  return saveTextToFile(() => ipc.exportSession(sessionId, format), {
    defaultFilename: exportFilename(title, format),
    extension: EXT[format],
    filterName: FILTER_NAME[format],
    mime: MIME[format],
  });
}
