// @vitest-environment jsdom
//
// #1184: the drain is only worth anything if it is actually installed on the
// close path, and only safe if a failure inside it still lets the window close.
// Registering a `tauri://close-requested` listener makes Tauri's Rust side call
// `api.prevent_close()`, so a handler that throws or hangs would leave the user
// with an app they cannot close — these cases pin both halves.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type CloseHandler = () => Promise<void>;

const { onCloseRequested, getCurrentWindow } = vi.hoisted(() => {
  const onCloseRequested = vi.fn(
    async (_handler: () => Promise<void>) => () => {},
  );
  return {
    onCloseRequested,
    getCurrentWindow: vi.fn(() => ({ onCloseRequested })),
  };
});

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow }));

const { flushDurableWrites } = vi.hoisted(() => ({
  flushDurableWrites: vi.fn(async () => {}),
}));

vi.mock("@/lib/durable-storage", () => ({ flushDurableWrites }));

function setTauri(on: boolean) {
  const w = globalThis.window as { __TAURI_INTERNALS__?: unknown };
  if (on) w.__TAURI_INTERNALS__ = {};
  else delete w.__TAURI_INTERNALS__;
}

beforeEach(() => {
  getCurrentWindow.mockClear();
  onCloseRequested.mockClear();
  flushDurableWrites.mockClear();
  flushDurableWrites.mockImplementation(async () => {});
  vi.resetModules();
});

afterEach(() => setTauri(false));

describe("installDurableFlush (#1184)", () => {
  it("drains on close inside Tauri", async () => {
    setTauri(true);
    const { installDurableFlush } = await import("@/lib/durable-flush");

    await installDurableFlush();
    expect(onCloseRequested).toHaveBeenCalledTimes(1);

    // Tauri awaits this handler before destroying the window, so awaiting the
    // drain inside it is what holds the close open until the bytes are through.
    const handler: CloseHandler = onCloseRequested.mock.calls[0][0];
    await handler();
    expect(flushDurableWrites).toHaveBeenCalledTimes(1);
  });

  it("does nothing outside Tauri", async () => {
    setTauri(false);
    const { installDurableFlush } = await import("@/lib/durable-flush");

    await installDurableFlush();
    // No IPC bridge, and `durableStorage` writes straight to localStorage —
    // there is nothing in flight to wait for.
    expect(getCurrentWindow).not.toHaveBeenCalled();
  });

  it("still lets the window close when the drain throws", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    flushDurableWrites.mockImplementation(async () => {
      throw new Error("store is wedged");
    });
    const { installDurableFlush } = await import("@/lib/durable-flush");

    await installDurableFlush();
    const handler: CloseHandler = onCloseRequested.mock.calls[0][0];

    // Must resolve, not reject: `preventDefault` was never called, so Tauri
    // destroys the window once this settles. A rejection here would strand it.
    await expect(handler()).resolves.toBeUndefined();
    expect(errorSpy).toHaveBeenCalled();
    errorSpy.mockRestore();
  });

  it("leaves the close path alone when the listener can't be registered", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    onCloseRequested.mockRejectedValueOnce(new Error("no bridge"));
    const { installDurableFlush } = await import("@/lib/durable-flush");

    // Swallowed: nothing prevented the close, so the app still quits normally —
    // we are just back to the pre-#1184 loss window.
    await expect(installDurableFlush()).resolves.toBeUndefined();
    expect(errorSpy).toHaveBeenCalled();
    errorSpy.mockRestore();
  });
});
