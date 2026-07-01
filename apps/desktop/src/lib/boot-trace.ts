// Boot-timing trace (#599 item 0), FE half. Reports first paint back to the Rust
// boot trace so all three cold-start milestones share one clock. The measurement
// logic is a pure function (`reportFirstPaintWith`) with every dependency injected
// so it is testable without a webview; `reportFirstPaint()` is the thin shell that
// wires the real runtime and is called once from App's mount effect.

import { invoke } from "@tauri-apps/api/core";

export interface FirstPaintDeps {
  /** The Tauri command invoker. */
  invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  /** True only inside a real Tauri webview -- never the mock/test runtime. */
  inTauri: boolean;
  /** Trace is enabled (dev build, or the `VITE_FF_BOOT_TRACE` flag). */
  enabled: boolean;
  /** Schedules a callback before the next repaint (real `requestAnimationFrame`). */
  raf: (cb: () => void) => void;
  /** Webview-internal clock (`performance.now()` ~= navigation start -> now). */
  now: () => number;
}

/** Pure core: when in a real webview with the trace on, report first paint after
 *  the browser has painted a frame (double-`raf`), passing the webview-internal
 *  elapsed so Rust can log both its own delta and the platform-floor proxy. */
export function reportFirstPaintWith(deps: FirstPaintDeps): void {
  if (!deps.inTauri || !deps.enabled) return;
  deps.raf(() =>
    deps.raf(() => {
      void deps
        .invoke("mark_fe_ready", { phase: "first-render", feNavMs: deps.now() })
        .catch(() => {});
    }),
  );
}

let reported = false;

/** Thin shell: wires the real runtime deps and fires at most once. */
export function reportFirstPaint(): void {
  if (reported) return;
  reported = true;
  const inTauri =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const enabled =
    import.meta.env.DEV || import.meta.env.VITE_FF_BOOT_TRACE === "1";
  const raf =
    typeof requestAnimationFrame === "function"
      ? (cb: () => void) => void requestAnimationFrame(() => cb())
      : (cb: () => void) => void setTimeout(cb, 0);
  reportFirstPaintWith({
    invoke,
    inTauri,
    enabled,
    raf,
    now: () => performance.now(),
  });
}
