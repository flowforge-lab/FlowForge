// About section constants, FE-owned contract types, and helpers (SET.11).
// The result types below have no ts-rs bindings yet (mock-only) — see the
// CONTRACT NOTE in lib/ipc.ts and #159.

/** Shown after the version in Settings → About. */
export const APP_TAGLINE = "flow-state AI interface";

/** Fallback when Tauri metadata is unavailable (mock dev in a browser). */
export const APP_VERSION_FALLBACK = "0.1.0";

/** Result of an update check. The FE owns the user-facing copy (see
 *  `formatUpdateStatus`); the backend reports only the structured outcome. */
export type UpdateStatus =
  | { kind: "upToDate"; version: string }
  | { kind: "available"; version: string; notes: string | null };

/** Result of an export/restore backup action. */
export interface BackupResult {
  /** Path the backup was written to (export) or read from (restore). */
  path: string;
}

/** Result of the CLI.7 sidecar parity smoke-test (`run_sidecar_turn`). Mirrors
 *  the `serde_json::Value` the Rust command returns: the synthetic session id
 *  the sidecar events were emitted under, and the total event count received
 *  from the CLI's `--json` stdout. FE-owned — no ts-rs binding. */
export interface SidecarTurnResult {
  /** Synthetic session id the re-emitted `turn:*` events are tagged with. */
  session_id: string;
  /** Total `AgentEvent` lines parsed from the sidecar's stdout. */
  events: number;
}

/** FE-owned toast copy for an update-check result. */
export function formatUpdateStatus(status: UpdateStatus): string {
  return status.kind === "available"
    ? `Version ${status.version} is available.`
    : "You're on the latest version.";
}

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
