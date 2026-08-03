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

describe("MockIpc updatePhenotype (#530)", () => {
  it("rejects the immutable built-in default", async () => {
    const ipc = new MockIpc();
    await expect(
      ipc.updatePhenotype({
        name: "default",
        skills: [],
        mcpServers: [],
        egress: "open",
        preheat: [],
      }),
    ).rejects.toThrow(/immutable/);
  });

  it("rejects an unknown provider connection", async () => {
    const ipc = new MockIpc();
    await expect(
      ipc.updatePhenotype({
        name: "rust",
        skills: [],
        provider: "ghost-conn",
        mcpServers: [],
        egress: "open",
        preheat: [],
      }),
    ).rejects.toThrow(/unknown connection/);
  });

  it("upserts a brand-new phenotype (appears in listPhenotypes)", async () => {
    const ipc = new MockIpc();
    const saved = await ipc.updatePhenotype({
      name: "data-science",
      skills: [],
      provider: "openai",
      model: "gpt-4o",
      mcpServers: [],
      egress: "open",
      preheat: [],
    });
    expect(saved).toMatchObject({ name: "data-science", provider: "openai" });
    const names = (await ipc.listPhenotypes()).map((p) => p.name);
    expect(names).toContain("data-science");
  });

  it("overwrites an existing phenotype by name (lossless round-trip)", async () => {
    const ipc = new MockIpc();
    await ipc.updatePhenotype({
      name: "rust",
      skills: ["rust-debugging", "write-tests"],
      persona: "You are a meticulous Rust engineer.",
      provider: "ollama",
      model: "qwen2.5",
      mcpServers: [],
      egress: "open",
      preheat: [],
    });
    const rust = (await ipc.listPhenotypes()).find((p) => p.name === "rust");
    expect(rust).toMatchObject({ provider: "ollama", model: "qwen2.5" });
    // Untouched fields survive the write.
    expect(rust?.persona).toBe("You are a meticulous Rust engineer.");
    expect(rust?.skills).toEqual(["rust-debugging", "write-tests"]);
  });

  it("re-applies skills and emits skills:changed when the active phenotype is edited", async () => {
    const ipc = new MockIpc();
    await ipc.switchPhenotype("rust");
    const events: string[][] = [];
    await ipc.onSkillsChanged((e) => events.push(e.active));

    await ipc.updatePhenotype({
      name: "rust",
      skills: ["write-tests"],
      persona: "You are a meticulous Rust engineer.",
      mcpServers: [],
      egress: "open",
      preheat: [],
    });
    expect((await ipc.getPhenotype()).skills).toEqual(["write-tests"]);
    expect(events[events.length - 1]).toEqual(["write-tests"]);
  });

  it("does not emit for an edit to a non-active phenotype", async () => {
    const ipc = new MockIpc();
    const events: string[][] = [];
    await ipc.onSkillsChanged((e) => events.push(e.active));
    await ipc.updatePhenotype({
      name: "rust",
      skills: ["write-tests"],
      mcpServers: [],
      egress: "open",
      preheat: [],
    });
    expect(events).toHaveLength(0);
  });
});
