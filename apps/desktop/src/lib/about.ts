// About section constants + helpers (SET.11). FE-only — no backend types.

/** Shown after the version in Settings → About. */
export const APP_TAGLINE = "flow-state AI interface";

/** Fallback when Tauri metadata is unavailable (mock dev in a browser). */
export const APP_VERSION_FALLBACK = "0.1.0";

export const ABOUT_BUG_REPORT_URL =
  "https://github.com/flowforge-lab/FlowForge/issues/new";

/** Community link — update when a Slack invite URL is published. */
export const ABOUT_SLACK_URL =
  "https://github.com/flowforge-lab/FlowForge/discussions";

/** Read the app version from Tauri metadata, or the fallback in mock/browser. */
export async function getAppVersion(): Promise<string> {
  if (
    globalThis.window !== undefined &&
    "__TAURI_INTERNALS__" in globalThis.window
  ) {
    const { getVersion } = await import("@tauri-apps/api/app");
    return getVersion();
  }
  return APP_VERSION_FALLBACK;
}

/** Open a URL in the system browser (Tauri opener plugin or window.open). */
export async function openExternalUrl(url: string): Promise<void> {
  if (
    globalThis.window !== undefined &&
    "__TAURI_INTERNALS__" in globalThis.window
  ) {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
    return;
  }
  globalThis.open(url, "_blank", "noopener,noreferrer");
}
