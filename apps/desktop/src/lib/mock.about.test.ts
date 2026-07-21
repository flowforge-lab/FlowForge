import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

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

  it("returns a backup path for export and restore", async () => {
    const ipc = new MockIpc();
    expect((await ipc.exportBackup()).path).toBeTruthy();
    expect((await ipc.restoreBackup()).path).toBeTruthy();
  });
});
