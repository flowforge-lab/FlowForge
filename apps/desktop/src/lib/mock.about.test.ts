import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { UpdateStatus } from "./about";

describe("MockIpc about actions (SET.11)", () => {
  it("reports an up-to-date structured update status on the github channel", async () => {
    const ipc = new MockIpc();
    const status = await ipc.checkForUpdates("github");
    expect(status.kind).toBe("upToDate");
    expect(status.version).toBeTruthy();
  });

  it("offers a newer build on the local dogfood channel (#1033)", async () => {
    const ipc = new MockIpc();
    const status = await ipc.checkForUpdates("local");
    expect(status.kind).toBe("available");
    expect(status.version).toBeTruthy();
  });

  it("refuses to install an older build without the downgrade opt-in (#1034)", async () => {
    const ipc = new MockIpc();
    // Force the older-build branch the way `VITE_FF_MOCK_UPDATE=older` would; the
    // env constant is read at module load, so drive the guard through the status.
    (ipc as unknown as { lastUpdateStatus: UpdateStatus }).lastUpdateStatus = {
      kind: "olderAvailable",
      version: "0.0.0-dev.1700000000",
      notes: null,
    };
    await expect(
      ipc.installUpdate("local", "0.0.0-dev.1700000000"),
    ).rejects.toThrow(/older/);
    // Confirmed downgrade goes through (and streams progress like any install).
    await expect(
      ipc.installUpdate("local", "0.0.0-dev.1700000000", true),
    ).resolves.toBeUndefined();
  });

  it("refuses to install a version the feed no longer offers (#1034)", async () => {
    const ipc = new MockIpc();
    // What the user confirmed (.1700) is not what the feed now serves (.1900) —
    // installing it anyway would install a build they never saw. Newer direction,
    // and the downgrade opt-in, are both irrelevant here.
    (ipc as unknown as { lastUpdateStatus: UpdateStatus }).lastUpdateStatus = {
      kind: "available",
      version: "0.0.0-dev.1900000000",
      notes: null,
    };
    await expect(
      ipc.installUpdate("local", "0.0.0-dev.1700000000", true),
    ).rejects.toThrow(/feed moved/);
    await expect(
      ipc.installUpdate("local", "0.0.0-dev.1900000000"),
    ).resolves.toBeUndefined();
  });

  it("returns a backup path for export and restore", async () => {
    const ipc = new MockIpc();
    expect((await ipc.exportBackup()).path).toBeTruthy();
    expect((await ipc.restoreBackup()).path).toBeTruthy();
  });
});
