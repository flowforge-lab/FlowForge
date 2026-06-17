import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import { memoryFilesFooter } from "./memory";

describe("MockIpc memory (SET.8)", () => {
  it("returns a cloned snapshot with WHO / HOW / WHAT categories", async () => {
    const ipc = new MockIpc();
    const snap = await ipc.getMemory();
    expect(snap.categories.who.subtitle).toMatch(/Role/i);
    expect(snap.categories.how.subtitle).toMatch(/Patterns/i);
    expect(snap.categories.what.subtitle).toMatch(/priorities/i);
    expect(snap.categories.who.items.length).toBeGreaterThan(0);
    expect(snap.journal.length).toBeGreaterThan(0);
    expect(snap.files.length).toBeGreaterThan(0);
  });

  it("does not alias the stored snapshot (mutating a result is isolated)", async () => {
    const ipc = new MockIpc();
    const snap = await ipc.getMemory();
    snap.files.push({ name: "leak.md", sizeBytes: 1 });
    const reread = await ipc.getMemory();
    expect(reread.files.some((f) => f.name === "leak.md")).toBe(false);
  });

  it("searchMemory filters journal, files, and categories", async () => {
    const ipc = new MockIpc();
    const full = await ipc.getMemory();
    const filtered = await ipc.searchMemory("RFC");
    expect(filtered.journal.length).toBeLessThanOrEqual(full.journal.length);
    expect(filtered.categories.what.items.some((i) => /RFC/i.test(i))).toBe(
      true,
    );
    expect(await ipc.searchMemory("nomatch-xyz")).toMatchObject({
      journal: [],
      files: [],
    });
  });

  it("files footer count and summed size match the mock catalog", async () => {
    const ipc = new MockIpc();
    const { count, totalBytes } = memoryFilesFooter(
      (await ipc.getMemory()).files,
    );
    expect(count).toBe(4);
    expect(totalBytes).toBe(2048 + 512 + 768 + 640);
  });
});
