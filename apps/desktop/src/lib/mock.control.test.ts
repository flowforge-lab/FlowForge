import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import { CONTROL_DEFAULTS } from "./control";

describe("MockIpc control config", () => {
  it("defaults with memory injection on and no prompt files", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getControlConfig();
    expect(cfg.injectMemory).toBe(true);
    expect(cfg.promptFiles).toEqual([]);
  });

  // Default mode is owned by the backend `mode.json` (#798), not the control config.
  it("exposes the global default mode separately, defaulting to auto", async () => {
    const ipc = new MockIpc();
    expect(await ipc.getDefaultMode()).toBe("auto");
    await ipc.setDefaultMode("act");
    expect(await ipc.getDefaultMode()).toBe("act");
  });

  it("seeds Team + UI defaults (SET.12)", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getControlConfig();
    expect(cfg.teammates.length).toBeGreaterThan(0);
    expect(cfg.ui.accentColor).toBe("#6366f1");
    expect(cfg.ui.contextualGreeting).toBe(true);
  });

  it("round-trips Team + UI changes (SET.12)", async () => {
    const ipc = new MockIpc();
    const base = await ipc.getControlConfig();
    await ipc.setControlConfig({
      ...base,
      teammates: [
        { id: "x", name: "Quinn", slug: "qa", description: "Tests." },
      ],
      ui: { ...base.ui, accentColor: "#10b981", logoPath: "/tmp/logo.png" },
    });

    const reread = await ipc.getControlConfig();
    expect(reread.teammates).toEqual([
      { id: "x", name: "Quinn", slug: "qa", description: "Tests." },
    ]);
    expect(reread.ui.accentColor).toBe("#10b981");
    expect(reread.ui.logoPath).toBe("/tmp/logo.png");
  });

  it("persists a written config and echoes it back on reopen", async () => {
    const ipc = new MockIpc();
    const next = {
      ...CONTROL_DEFAULTS,
      injectMemory: false,
      userInstructions: "Be terse.",
      promptFiles: ["{workspace}/AGENTS.md"],
    };
    const stored = await ipc.setControlConfig(next);
    expect(stored.injectMemory).toBe(false);

    const reread = await ipc.getControlConfig();
    expect(reread.userInstructions).toBe("Be terse.");
    expect(reread.promptFiles).toEqual(["{workspace}/AGENTS.md"]);
    expect(reread.injectMemory).toBe(false);
  });

  it("does not alias the stored config (mutating a result is isolated)", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getControlConfig();
    cfg.promptFiles.push("leak.md");
    const reread = await ipc.getControlConfig();
    expect(reread.promptFiles).toEqual([]);
  });
});
