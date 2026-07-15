import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc files panel (#872)", () => {
  it("lists the root: directories first, then case-insensitive alphabetical", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const entries = await ipc.listDirectory(s.id, "");
    const names = entries.map((e) => e.name);
    // Dirs (assets, docs, src) before files (package.json, README.md).
    expect(names).toEqual([
      "assets",
      "docs",
      "src",
      "package.json",
      "README.md",
    ]);
    expect(entries.slice(0, 3).every((e) => e.isDir)).toBe(true);
    expect(entries.slice(3).every((e) => !e.isDir)).toBe(true);
  });

  it("lists a subdirectory and reports nested dirs", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const names = (await ipc.listDirectory(s.id, "src")).map((e) => e.name);
    expect(names).toEqual(["lib", "app.ts", "main.ts"]);
  });

  it("does not expose gitignored dirs like node_modules", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const names = (await ipc.listDirectory(s.id, "")).map((e) => e.name);
    expect(names).not.toContain("node_modules");
  });

  it("rejects a path that escapes the workspace root", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await expect(ipc.listDirectory(s.id, "../etc")).rejects.toThrow(
      "access denied",
    );
    await expect(ipc.readFile(s.id, "../secret.txt")).rejects.toThrow(
      "access denied",
    );
  });

  it("reads a text file as UTF-8", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const fc = await ipc.readFile(s.id, "package.json");
    expect(fc.isBinary).toBe(false);
    expect(fc.truncated).toBe(false);
    expect(fc.text).toContain('"name": "flowforge"');
    expect(fc.size).toBeGreaterThan(0);
  });

  it("flags a binary file with no text", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const fc = await ipc.readFile(s.id, "assets/logo.png");
    expect(fc.isBinary).toBe(true);
    expect(fc.text).toBeNull();
  });

  it("truncates a file to the requested byte cap", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const fc = await ipc.readFile(s.id, "README.md", 8);
    expect(fc.truncated).toBe(true);
    expect(fc.text).toBe("# FlowFo");
    expect(fc.size).toBeGreaterThan(8);
  });

  it("throws for a missing file", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await expect(ipc.readFile(s.id, "nope.txt")).rejects.toThrow(
      "no such file",
    );
  });
});
