// Drain pending durable writes before the window goes away (#1184).
//
// `writeDurable` and zustand's `persist` both discard the promise `setItem`
// returns, so a write issued by the last action before a close can be lost in
// the gap between the action returning and the write settling.
// `durable-storage` now tracks those promises; this module is the one place
// that waits on them.
//
// Why the window-close event and not an exit hook: on macOS ⌘Q is muda's
// predefined Quit → `terminate:` → `applicationWillTerminate` → tao's
// `AppState::exit` → `Event::LoopDestroyed` → `RunEvent::Exit`. Neither
// `WindowEvent::CloseRequested` nor `RunEvent::ExitRequested` is emitted on that
// path, so no JS hook exists there at all. ⌘Q is covered instead by
// `tauri-plugin-store`'s own `RunEvent::Exit` handler, which saves every loaded
// store — so a `set` that reached Rust still lands on disk. What this closes is
// the window-close path, where JS *can* wait.

import { flushDurableWrites } from "./durable-storage";

function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

/** Register the teardown drain. No-op outside Tauri (browser / `dev:mock` /
 *  tests), where `durableStorage` writes to `localStorage` synchronously and
 *  there is nothing in flight to wait for.
 *
 *  Two Tauri behaviours make this work without touching `preventDefault`:
 *  registering a JS listener for `tauri://close-requested` makes the Rust side
 *  call `api.prevent_close()` on its own, and `onCloseRequested` awaits an async
 *  handler before destroying the window. So awaiting the drain here holds the
 *  close exactly as long as the writes need, then lets it proceed.
 *
 *  That also means a handler that throws or never returns would leave the user
 *  with a window that won't close. `flushDurableWrites` is bounded, and the
 *  `catch` below closes anyway: a lost write is recoverable, an unclosable app
 *  is not. */
export async function installDurableFlush(): Promise<void> {
  if (!inTauri()) return;

  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().onCloseRequested(async () => {
      try {
        await flushDurableWrites();
      } catch (err) {
        console.error("[durableFlush] drain failed; closing anyway", err);
      }
    });
  } catch (err) {
    // Couldn't subscribe. Nothing prevented the close, so the app still quits
    // normally — we're just back to the pre-#1184 loss window.
    console.error("[durableFlush] could not register the close hook", err);
  }
}
