import { beforeEach, describe, expect, it } from "vitest";

import { useScheduledStore } from "@/store/scheduled";

describe("useScheduledStore", () => {
  beforeEach(() => {
    useScheduledStore.setState({
      tasks: [],
      loading: false,
      saving: false,
      error: null,
    });
  });

  it("loads tasks including the Memory Organizer builtin", async () => {
    await useScheduledStore.getState().load();
    const { tasks } = useScheduledStore.getState();
    expect(tasks.length).toBeGreaterThanOrEqual(1);
    const builtin = tasks.find((t) => t.id === "memory-organizer");
    expect(builtin?.builtin).toBe(true);
  });

  it("toggle flips a task's paused state", async () => {
    await useScheduledStore.getState().load();
    const before = useScheduledStore
      .getState()
      .tasks.find((t) => t.id === "memory-organizer")!;

    await useScheduledStore.getState().toggle("memory-organizer");
    const after = useScheduledStore
      .getState()
      .tasks.find((t) => t.id === "memory-organizer")!;
    expect(after.paused).toBe(!before.paused);
  });

  it("create appends a user task to the list", async () => {
    await useScheduledStore.getState().load();
    const countBefore = useScheduledStore.getState().tasks.length;

    await useScheduledStore.getState().create({
      name: "Nightly Backup",
      cron: "0 2 * * *",
      cadenceLabel: "Daily at 2:00 AM",
    });
    const tasks = useScheduledStore.getState().tasks;
    expect(tasks.length).toBe(countBefore + 1);
    expect(tasks[tasks.length - 1].name).toBe("Nightly Backup");
  });

  it("resetScheduled resumes every paused task", async () => {
    await useScheduledStore.getState().load();
    // Pause a task so there's something to reset.
    const id = useScheduledStore.getState().tasks[0].id;
    if (!useScheduledStore.getState().tasks[0].paused) {
      await useScheduledStore.getState().toggle(id);
    }
    expect(useScheduledStore.getState().tasks.some((t) => t.paused)).toBe(true);

    await useScheduledStore.getState().resetScheduled();
    expect(useScheduledStore.getState().tasks.every((t) => !t.paused)).toBe(
      true,
    );
  });
});
