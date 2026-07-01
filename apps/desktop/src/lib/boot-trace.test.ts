import { describe, expect, it, vi } from "vitest";
import { reportFirstPaintWith, type FirstPaintDeps } from "./boot-trace";

/** A `raf` that runs its callback synchronously, so a double-`raf` chain resolves
 *  in-line during the test. */
const syncRaf = (cb: () => void) => cb();

function deps(over: Partial<FirstPaintDeps> = {}): FirstPaintDeps {
  return {
    invoke: vi.fn().mockResolvedValue(undefined),
    inTauri: true,
    enabled: true,
    raf: syncRaf,
    now: () => 42,
    ...over,
  };
}

describe("reportFirstPaintWith (#599)", () => {
  it("invokes mark_fe_ready with the webview-internal timestamp when in a webview and enabled", () => {
    const d = deps();
    reportFirstPaintWith(d);
    expect(d.invoke).toHaveBeenCalledTimes(1);
    expect(d.invoke).toHaveBeenCalledWith("mark_fe_ready", {
      phase: "first-render",
      feNavMs: 42,
    });
  });

  it("fires only after a double rAF (one frame is not enough)", () => {
    let depth = 0;
    const oneFrame = (cb: () => void) => {
      if (depth === 0) {
        depth++;
        cb();
      }
      // A nested rAF at depth 1 is dropped — simulates only one painted frame.
    };
    const d = deps({ raf: oneFrame });
    reportFirstPaintWith(d);
    expect(d.invoke).not.toHaveBeenCalled();
  });

  it("no-ops outside a Tauri webview (mock/test runtime)", () => {
    const d = deps({ inTauri: false });
    reportFirstPaintWith(d);
    expect(d.invoke).not.toHaveBeenCalled();
  });

  it("no-ops when the trace flag is off", () => {
    const d = deps({ enabled: false });
    reportFirstPaintWith(d);
    expect(d.invoke).not.toHaveBeenCalled();
  });

  it("swallows an invoke rejection (a failed trace must never break boot)", async () => {
    const d = deps({ invoke: vi.fn().mockRejectedValue(new Error("no host")) });
    expect(() => reportFirstPaintWith(d)).not.toThrow();
    await Promise.resolve();
  });
});
