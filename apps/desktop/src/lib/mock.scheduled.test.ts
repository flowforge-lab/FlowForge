import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc scheduled tasks (SET.9)", () => {
  it("lists a built-in Memory Organizer task", async () => {
    const ipc = new MockIpc();
    const tasks = await ipc.listScheduledTasks();
    const builtin = tasks.find((t) => t.id === "memory-organizer");
    expect(builtin?.builtin).toBe(true);
    expect(builtin?.cadenceLabel).toBe("Daily at 5:00 PM");
  });

  it("toggles pause/resume and clears nextRun while paused", async () => {
    const ipc = new MockIpc();
    const paused = await ipc.toggleScheduledTask("memory-organizer");
    expect(paused.paused).toBe(true);
    expect(paused.nextRun).toBeNull();

    const resumed = await ipc.toggleScheduledTask("memory-organizer");
    expect(resumed.paused).toBe(false);
    expect(resumed.nextRun).not.toBeNull();
  });

  it("rejects toggling an unknown task", async () => {
    const ipc = new MockIpc();
    await expect(ipc.toggleScheduledTask("ghost")).rejects.toThrow("unknown");
  });

  it("creates a session-persistent user task", async () => {
    const ipc = new MockIpc();
    const created = await ipc.createScheduledTask({
      name: "Nightly Backup",
      cron: "0 2 * * *",
      cadenceLabel: "Daily at 2:00 AM",
    });
    expect(created.builtin).toBe(false);
    expect(created.id).toBeTruthy();

    const tasks = await ipc.listScheduledTasks();
    expect(tasks.some((t) => t.id === created.id)).toBe(true);
  });
});
