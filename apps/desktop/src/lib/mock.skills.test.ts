import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc skill discovery", () => {
  it("lists all skills name-sorted with score 0", async () => {
    const ipc = new MockIpc();
    const skills = await ipc.listSkills();
    const names = skills.map((s) => s.name);
    expect(names).toEqual([...names].sort());
    expect(skills.every((s) => s.score === 0)).toBe(true);
    expect(skills.every((s) => s.active === false)).toBe(true);
  });

  it("ranks search results: empty query lists all, query filters and ranks", async () => {
    const ipc = new MockIpc();
    expect((await ipc.searchSkills("")).length).toEqual(
      (await ipc.listSkills()).length,
    );

    const hits = await ipc.searchSkills("rust");
    expect(hits[0].name).toBe("rust-debugging");
    expect(hits.every((h) => h.score > 0)).toBe(true);
    expect(await ipc.searchSkills("nomatch-xyz")).toEqual([]);
  });

  it("activate toggles active state and emits skills:changed", async () => {
    const ipc = new MockIpc();
    const events: string[][] = [];
    await ipc.onSkillsChanged((e) => events.push(e.active));

    await ipc.activateSkill("rust-debugging");
    expect(events[events.length - 1]).toEqual(["rust-debugging"]);
    expect(
      (await ipc.listSkills()).find((s) => s.name === "rust-debugging")?.active,
    ).toBe(true);

    await ipc.deactivateSkill("rust-debugging");
    expect(events[events.length - 1]).toEqual([]);
  });

  it("rejects activating an unknown skill", async () => {
    const ipc = new MockIpc();
    await expect(ipc.activateSkill("ghost")).rejects.toThrow("unknown skill");
  });
});

describe("MockIpc skill marketplace (SET.5)", () => {
  it("lists the full catalog by install count for an empty query", async () => {
    const ipc = new MockIpc();
    const results = await ipc.searchSkillMarketplace("");
    expect(results.length).toBeGreaterThan(1);
    const installs = results.map((r) => r.installs);
    expect(installs).toEqual([...installs].sort((a, b) => b - a));
  });

  it("ranks matches and drops non-matches for a query", async () => {
    const ipc = new MockIpc();
    const hits = await ipc.searchSkillMarketplace("pdf");
    expect(hits[0].name).toBe("pdf-toolkit");
    expect(hits.every((h) => h.name === "pdf-toolkit")).toBe(true);

    expect(await ipc.searchSkillMarketplace("nomatch-xyz")).toEqual([]);
  });
});
