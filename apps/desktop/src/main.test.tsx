// @vitest-environment jsdom
//
// #1184: `main.tsx` is the drain's only entry point. `durable-storage.test.ts`
// pins what the drain *does* and `durable-flush.test.ts` pins what the close
// hook does, but neither notices if nobody ever calls `installDurableFlush` —
// delete that one line and the whole feature is inert with a fully green suite.
// One careless rebase is all it takes, so the call itself gets a test.

import { beforeEach, describe, expect, it, vi } from "vitest";

const { installDurableFlush } = vi.hoisted(() => ({
  installDurableFlush: vi.fn(async () => {}),
}));

vi.mock("@/lib/durable-flush", () => ({ installDurableFlush }));

// Everything below is boot machinery that isn't under test; importing `main.tsx`
// runs it for real otherwise (mounting the whole app, opening IPC, hydrating
// every store).
const { render } = vi.hoisted(() => ({ render: vi.fn() }));
vi.mock("react-dom/client", () => ({
  default: { createRoot: () => ({ render }) },
  createRoot: () => ({ render }),
}));
vi.mock("@/App", () => ({ default: () => null }));

beforeEach(() => {
  installDurableFlush.mockClear();
  render.mockClear();
  document.body.innerHTML = '<div id="root"></div>';
  vi.resetModules();
});

describe("main.tsx wiring (#1184)", () => {
  it("installs the durable-write drain on boot", async () => {
    await import("./main");

    expect(installDurableFlush).toHaveBeenCalledTimes(1);
  });

  it("still mounts the app", async () => {
    // Guards the obvious way to "fix" a failure of the case above: reordering
    // or wrapping the boot sequence such that the drain installs but the render
    // no longer happens.
    await import("./main");

    expect(render).toHaveBeenCalledTimes(1);
  });
});
