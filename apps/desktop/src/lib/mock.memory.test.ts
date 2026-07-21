import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import { parseCategories } from "./memory-view";

describe("MockIpc memory (M5.1e, #131)", () => {
  it("lists curated first, then daily newest-first, with no body leaked", async () => {
    const ipc = new MockIpc();
    const files = await ipc.listMemoryFiles();

    expect(files.map((f) => f.relPath)).toEqual([
      "MEMORY.md",
      "daily/2026-06-18.md",
      "daily/2026-06-17.md",
    ]);
    expect(files[0].kind).toBe("curated");
    expect(files.slice(1).every((f) => f.kind === "daily")).toBe(true);
    // Daily files are newest-first by modified time.
    expect(files[1].modifiedMs).toBeGreaterThan(files[2].modifiedMs);
    // The listing shape carries metadata only — never the file body.
    expect(files.every((f) => !("body" in f))).toBe(true);
  });

  it("overview counts and bytes match the listed files", async () => {
    const ipc = new MockIpc();
    const files = await ipc.listMemoryFiles();
    const overview = await ipc.memoryOverview();

    expect(overview.enabled).toBe(true);
    expect(overview.fileCount).toBe(files.length);
    expect(overview.totalBytes).toBe(
      files.reduce((sum, f) => sum + f.sizeBytes, 0),
    );
    expect(overview.rootPath.length).toBeGreaterThan(0);
  });

  it("reads a file body round-trip and rejects an unknown path", async () => {
    const ipc = new MockIpc();
    const body = await ipc.readMemoryFile("MEMORY.md");
    expect(body).toContain("# Memory");

    const daily = await ipc.readMemoryFile("daily/2026-06-18.md");
    expect(daily).toContain("memory IPC contract");

    await expect(ipc.readMemoryFile("daily/ghost.md")).rejects.toThrow(
      "invalid memory path",
    );
  });
});

describe("MockIpc — curated write seam (#868)", () => {
  it("replaces a stratum body in place and re-reads it", async () => {
    const ipc = new MockIpc();
    await ipc.writeCuratedMemory("focus", "Editable memory strata.");

    const body = await ipc.readMemoryFile("MEMORY.md");
    const cats = parseCategories(body);
    expect(cats.focus).toBe("Editable memory strata.");
    // Siblings and the file title survive the write.
    expect(cats.identity).toContain("frontend");
    expect(body.startsWith("# Memory")).toBe(true);
  });

  it("creates a missing heading rather than dropping the write", async () => {
    // Mirrors the backend's `replace_curated_stratum_creates_when_absent`: the
    // mock's fixture has all three strata, so drop one first.
    const ipc = new MockIpc();
    await ipc.writeCuratedMemory("focus", "");
    const cleared = parseCategories(await ipc.readMemoryFile("MEMORY.md"));
    expect(cleared.focus).toBe("");

    await ipc.writeCuratedMemory("focus", "Shipping #868.");
    const body = await ipc.readMemoryFile("MEMORY.md");
    expect(parseCategories(body).focus).toBe("Shipping #868.");
    expect(body.match(/## Focus/g)).toHaveLength(1);
  });

  it("keeps the listing metadata consistent with the new body", async () => {
    const ipc = new MockIpc();
    await ipc.writeCuratedMemory("identity", "short");

    const files = await ipc.listMemoryFiles();
    const curated = files.find((f) => f.kind === "curated");
    const body = await ipc.readMemoryFile("MEMORY.md");
    expect(curated?.sizeBytes).toBe(new TextEncoder().encode(body).length);

    const overview = await ipc.memoryOverview();
    expect(overview.totalBytes).toBe(
      files.reduce((sum, f) => sum + f.sizeBytes, 0),
    );
  });
});
