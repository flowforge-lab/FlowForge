import { beforeEach, describe, expect, it } from "vitest";

import { useControlConfigStore } from "@/store/control-config";
import { policyForMode } from "@/lib/control";

// The store talks to the shared MockIpc singleton; load + reset to a clean
// baseline before each test.
beforeEach(async () => {
  await useControlConfigStore.getState().load();
  await useControlConfigStore.getState().resetControl();
});

const cfg = () => useControlConfigStore.getState().config!;

describe("control-config store", () => {
  it("loads the config from IPC", () => {
    expect(cfg().defaultMode).toBe("auto");
  });

  it("setDefaultMode updates the mode and re-derives the policy", async () => {
    await useControlConfigStore.getState().setDefaultMode("act");
    expect(cfg().defaultMode).toBe("act");
    expect(cfg().permissionPolicy).toEqual(policyForMode("act"));
  });

  it("adds and removes overrides, ignoring blanks and dupes", async () => {
    await useControlConfigStore.getState().addOverride("denied", "rm -rf");
    await useControlConfigStore.getState().addOverride("denied", "rm -rf"); // dupe
    await useControlConfigStore.getState().addOverride("denied", "  "); // blank
    expect(cfg().overrides.denied).toEqual(["rm -rf"]);

    await useControlConfigStore.getState().removeOverride("denied", "rm -rf");
    expect(cfg().overrides.denied).toEqual([]);
  });

  it("manages prompt files and toggles injectMemory + userInstructions", async () => {
    await useControlConfigStore
      .getState()
      .addPromptFile("{workspace}/AGENTS.md");
    await useControlConfigStore
      .getState()
      .addPromptFile("{workspace}/AGENTS.md"); // dupe
    expect(cfg().promptFiles).toEqual(["{workspace}/AGENTS.md"]);

    await useControlConfigStore.getState().setInjectMemory(false);
    await useControlConfigStore.getState().setUserInstructions("Be terse.");
    expect(cfg().injectMemory).toBe(false);
    expect(cfg().userInstructions).toBe("Be terse.");

    await useControlConfigStore
      .getState()
      .removePromptFile("{workspace}/AGENTS.md");
    expect(cfg().promptFiles).toEqual([]);
  });

  it("persists through IPC (a fresh load echoes the change)", async () => {
    await useControlConfigStore.getState().setDefaultMode("plan");
    await useControlConfigStore.getState().load();
    expect(cfg().defaultMode).toBe("plan");
  });

  it("resetControl restores defaults", async () => {
    await useControlConfigStore.getState().setDefaultMode("act");
    await useControlConfigStore.getState().addOverride("allowed", "ls");
    await useControlConfigStore.getState().resetControl();
    expect(cfg().defaultMode).toBe("auto");
    expect(cfg().overrides.allowed).toEqual([]);
  });
});
