import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import { CONTROL_DEFAULTS } from "./control";

describe("MockIpc control config", () => {
  it("defaults to auto mode with memory injection on", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getControlConfig();
    expect(cfg.defaultMode).toBe("auto");
    expect(cfg.injectMemory).toBe(true);
    expect(cfg.promptFiles).toEqual([]);
  });

  it("persists a written config and echoes it back on reopen", async () => {
    const ipc = new MockIpc();
    const next = {
      ...CONTROL_DEFAULTS,
      defaultMode: "act" as const,
      injectMemory: false,
      userInstructions: "Be terse.",
      promptFiles: ["{workspace}/AGENTS.md"],
      overrides: { denied: ["rm"], requireApproval: [], allowed: [] },
    };
    const stored = await ipc.setControlConfig(next);
    expect(stored.defaultMode).toBe("act");

    const reread = await ipc.getControlConfig();
    expect(reread.userInstructions).toBe("Be terse.");
    expect(reread.promptFiles).toEqual(["{workspace}/AGENTS.md"]);
    expect(reread.overrides.denied).toEqual(["rm"]);
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
