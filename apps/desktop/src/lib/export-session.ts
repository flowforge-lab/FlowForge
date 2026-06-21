// Session export (#278). A thin client over the backend `export_session` command:
// it asks the backend for the serialized string (lossless JSON or Markdown), then
// writes it to a user-chosen path. Inside Tauri that's a native save dialog +
// `plugin-fs` write; in a plain browser (mock/dev) it falls back to a Blob
// download so the flow is exercisable under `VITE_FF_MOCK=1`.

import { ipc } from "@/lib/ipc";
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

/** Evaluated per call (not a module constant) so tests can toggle the branch. */
function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

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

export type ExportResult =
  | { status: "saved"; path: string }
  | { status: "cancelled" };

/**
 * Export `sessionId` as `format`, writing to a user-chosen file. In Tauri: a save
 * dialog (cancel → `{ status: "cancelled" }`) then a `plugin-fs` write. In a
 * browser: a Blob download. Throws if the backend export or the write fails — the
 * caller surfaces that as an error toast.
 */
export async function exportSessionToFile(
  sessionId: string,
  title: string | null | undefined,
  format: Format,
): Promise<ExportResult> {
  const filename = exportFilename(title, format);

  if (inTauri()) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: filename,
      filters: [{ name: FILTER_NAME[format], extensions: [EXT[format]] }],
    });
    if (!path) return { status: "cancelled" };
    const content = await ipc.exportSession(sessionId, format);
    const { writeTextFile } = await import("@tauri-apps/plugin-fs");
    await writeTextFile(path, content);
    return { status: "saved", path };
  }

  // Browser / mock: synthesize a download with the default filename.
  const content = await ipc.exportSession(sessionId, format);
  const url = URL.createObjectURL(new Blob([content], { type: MIME[format] }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
  return { status: "saved", path: filename };
}
