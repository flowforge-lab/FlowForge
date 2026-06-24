// Shared "write a string to a user-chosen file" core (#278/#417). Inside Tauri that's
// a native save dialog + `plugin-fs` write; in a plain browser (mock/dev) it falls back
// to a Blob download so the flow is exercisable under `VITE_FF_MOCK=1`. Extracted from
// export-session.ts so the session export and the step-timeline export (#417) share one
// implementation — no duplicated dialog/blob logic.

export type SaveResult =
  | { status: "saved"; path: string }
  | { status: "cancelled" };

export interface SaveOptions {
  /** Pre-filled name in the dialog / Blob download, e.g. `my-chat.md`. */
  defaultFilename: string;
  /** Extension (no dot) for the dialog filter, e.g. `csv`. */
  extension: string;
  /** Human filter label, e.g. `CSV`. */
  filterName: string;
  /** MIME type for the browser Blob fallback. */
  mime: string;
}

/** Evaluated per call (not a module constant) so tests can toggle the branch. */
function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

/**
 * Write `content` to a user-chosen file. `content` may be a string or a lazy provider;
 * the provider is invoked **only after** the save dialog is confirmed, so an expensive
 * source (e.g. a backend serialize) is never run when the user cancels.
 *
 * In Tauri: a save dialog (cancel → `{ status: "cancelled" }`) then a `plugin-fs` write.
 * In a browser: a Blob download (always "saved"). Throws if the provider or write fails —
 * the caller surfaces that as an error toast.
 */
export async function saveTextToFile(
  content: string | (() => string | Promise<string>),
  opts: SaveOptions,
): Promise<SaveResult> {
  const resolve = () =>
    typeof content === "function" ? content() : Promise.resolve(content);

  if (inTauri()) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: opts.defaultFilename,
      filters: [{ name: opts.filterName, extensions: [opts.extension] }],
    });
    if (!path) return { status: "cancelled" };
    const text = await resolve();
    const { writeTextFile } = await import("@tauri-apps/plugin-fs");
    await writeTextFile(path, text);
    return { status: "saved", path };
  }

  // Browser / mock: synthesize a download with the default filename.
  const text = await resolve();
  const url = URL.createObjectURL(new Blob([text], { type: opts.mime }));
  const a = document.createElement("a");
  a.href = url;
  a.download = opts.defaultFilename;
  a.click();
  URL.revokeObjectURL(url);
  return { status: "saved", path: opts.defaultFilename };
}
