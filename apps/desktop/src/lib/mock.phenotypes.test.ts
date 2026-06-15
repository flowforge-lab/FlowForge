import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc phenotypes", () => {
  it("lists the built-in default first", async () => {
    const ipc = new MockIpc();
    const names = (await ipc.listPhenotypes()).map((p) => p.name);
    expect(names[0]).toBe("default");
    expect(names).toContain("rust");
  });

  it("defaults to the built-in phenotype with no active skills", async () => {
    const ipc = new MockIpc();
    expect((await ipc.getPhenotype()).name).toBe("default");
    expect((await ipc.listSkills()).every((s) => !s.active)).toBe(true);
  });

  it("switch replaces the active set and emits skills:changed", async () => {
    const ipc = new MockIpc();
    const events: string[][] = [];
    await ipc.onSkillsChanged((e) => events.push(e.active));

    const pheno = await ipc.switchPhenotype("rust");
    expect(pheno.name).toBe("rust");
    expect((await ipc.getPhenotype()).name).toBe("rust");
    // Active set is the phenotype's installed skills, name-sorted.
    expect(events[events.length - 1]).toEqual([
      "rust-debugging",
      "write-tests",
    ]);

    // Switching again replaces (not unions) the active set.
    await ipc.switchPhenotype("reviewer");
    expect(events[events.length - 1]).toEqual(["create-pr"]);
  });

  it("rejects an unknown phenotype", async () => {
    const ipc = new MockIpc();
    await expect(ipc.switchPhenotype("ghost")).rejects.toThrow(
      "unknown phenotype",
    );
  });
});
