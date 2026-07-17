// Focus-aware document.title flash (#994). When a notification fires while the app
// window is unfocused/backgrounded (another app, another tab), an in-app toast alone
// isn't visible — so we also flash the window/tab title, e.g. "● Needs approval —
// FlowForge". The moment the user returns (focus / visibilitychange), the base title
// is restored. No permission prompt, no bundle cost, reliable in the Tauri webview —
// unlike the Web Notification API. Gated behind the master notifications toggle by the
// caller (lib/notify.ts).

import type { ToastKind } from "@/store/session-toast";

/** Short prefix shown before the base title while flashing, per kind. */
const FLASH_LABEL: Record<ToastKind, string> = {
  done: "Finished",
  error: "Failed",
  approval: "Needs approval",
  stopped: "Stopped",
};

let baseTitle = "";
let flashing = false;
let initialized = false;

/** True while the window is focused/visible — no flash needed. Also true when
 *  there's no DOM (tests / non-browser), so the flash is a safe no-op there. */
function isForeground(): boolean {
  if (typeof document === "undefined") return true;
  return document.visibilityState === "visible" && document.hasFocus();
}

function restore(): void {
  if (!flashing) return;
  flashing = false;
  document.title = baseTitle;
}

/** Capture the base title and wire the restore listeners. Idempotent; call once on
 *  app mount (App.tsx, beside initPrefs). */
export function initTitleFlash(): void {
  if (initialized) return;
  if (typeof document === "undefined" || typeof window === "undefined") return;
  initialized = true;
  baseTitle = document.title;
  // Any return-to-foreground clears the flash. Both events fire across the paths
  // that matter (tab switch → visibilitychange; window focus → focus).
  window.addEventListener("focus", restore);
  document.addEventListener("visibilitychange", () => {
    if (isForeground()) restore();
  });
}

/** Flash the title for `kind` — a no-op while the window is in the foreground (the
 *  in-app toast already covers that case). Safe before init: refreshes the base
 *  title if it wasn't captured yet. */
export function flashTitle(kind: ToastKind): void {
  if (isForeground()) return;
  if (!flashing) {
    // Preserve whatever the current (non-flashing) title is as the restore target.
    if (!baseTitle) baseTitle = document.title;
    flashing = true;
  }
  document.title = `● ${FLASH_LABEL[kind]} — ${baseTitle}`;
}

/** Test-only reset of module state. */
export function __resetTitleFlashForTest(): void {
  baseTitle = "";
  flashing = false;
  initialized = false;
}
