import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc scheduled tasks (RFC 0017)", () => {
  it("lists a built-in Memory Organizer task", async () => {
    const ipc = new MockIpc();
    const tasks = await ipc.listScheduledTasks();
    const builtin = tasks.find((t) => t.id === "memory-organizer");
    expect(builtin?.kind.kind).toBe("builtin");
    expect(builtin?.cadenceLabel).toBe("Daily at 5:00 PM");
  });

  it("toggles pause/resume and clears nextRun while paused", async () => {
    const ipc = new MockIpc();
    const paused = await ipc.toggleScheduledTask("memory-organizer");
    expect(paused.paused).toBe(true);
    expect(paused.nextRun).toBeUndefined();

    const resumed = await ipc.toggleScheduledTask("memory-organizer");
    expect(resumed.paused).toBe(false);
    expect(resumed.nextRun).not.toBeUndefined();
  });

  it("rejects toggling an unknown task", async () => {
    const ipc = new MockIpc();
    await expect(ipc.toggleScheduledTask("ghost")).rejects.toThrow("unknown");
  });

  it("creates a session-persistent user task", async () => {
    const ipc = new MockIpc();
    const created = await ipc.createScheduledTask({
      name: "Nightly Backup",
      cron: "0 0 2 * * *",
      kind: { kind: "prompt", value: "Back up the database." },
      safetyCeiling: "read_only",
    });
    expect(created.kind.kind).toBe("prompt");
    expect(created.id).toBeTruthy();

    const tasks = await ipc.listScheduledTasks();
    expect(tasks.some((t) => t.id === created.id)).toBe(true);
  });

  it("rejects deleting a built-in task", async () => {
    const ipc = new MockIpc();
    await expect(ipc.deleteScheduledTask("memory-organizer")).rejects.toThrow(
      "built-in",
    );
  });
});
