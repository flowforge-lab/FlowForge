import { beforeEach, describe, expect, it } from "vitest";

import { useScheduledStore } from "@/store/scheduled";

describe("useScheduledStore", () => {
  beforeEach(() => {
    useScheduledStore.setState({
      tasks: [],
      loading: false,
      saving: false,
      error: null,
      runsByTask: {},
      runningId: null,
    });
  });

  it("loads tasks including the Memory Organizer builtin", async () => {
    await useScheduledStore.getState().load();
    const { tasks } = useScheduledStore.getState();
    expect(tasks.length).toBeGreaterThanOrEqual(1);
    const builtin = tasks.find((t) => t.id === "memory-organizer");
    expect(builtin?.kind.kind).toBe("builtin");
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
      cron: "0 0 2 * * *",
      kind: { kind: "prompt", value: "Back up the database." },
      safetyCeiling: "read_only",
    });
    const tasks = useScheduledStore.getState().tasks;
    expect(tasks.length).toBe(countBefore + 1);
    expect(tasks[tasks.length - 1].name).toBe("Nightly Backup");
  });

  it("remove deletes a user task from the list", async () => {
    await useScheduledStore.getState().load();
    await useScheduledStore.getState().create({
      name: "Throwaway",
      cron: "0 0 2 * * *",
      kind: { kind: "prompt", value: "noop" },
      safetyCeiling: "read_only",
    });
    const created = useScheduledStore
      .getState()
      .tasks.find((t) => t.name === "Throwaway")!;

    await useScheduledStore.getState().remove(created.id);
    expect(
      useScheduledStore.getState().tasks.some((t) => t.id === created.id),
    ).toBe(false);
  });

  it("remove surfaces an error for a built-in task and keeps it", async () => {
    await useScheduledStore.getState().load();
    await useScheduledStore.getState().remove("memory-organizer");
    expect(useScheduledStore.getState().error).toBeTruthy();
    expect(
      useScheduledStore
        .getState()
        .tasks.some((t) => t.id === "memory-organizer"),
    ).toBe(true);
  });

  it("edit recreates the task in place with the new fields", async () => {
    await useScheduledStore.getState().load();
    await useScheduledStore.getState().create({
      name: "Before",
      cron: "0 0 2 * * *",
      kind: { kind: "prompt", value: "old" },
      safetyCeiling: "read_only",
    });
    const before = useScheduledStore
      .getState()
      .tasks.find((t) => t.name === "Before")!;
    const indexBefore = useScheduledStore
      .getState()
      .tasks.findIndex((t) => t.id === before.id);

    await useScheduledStore.getState().edit(before.id, {
      name: "After",
      cron: "0 0 9 * * 1",
      kind: { kind: "prompt", value: "new" },
      safetyCeiling: "write",
    });
    const tasks = useScheduledStore.getState().tasks;
    // Same slot, new identity + fields (delete + recreate).
    expect(tasks.some((t) => t.id === before.id)).toBe(false);
    expect(tasks[indexBefore].name).toBe("After");
    expect(tasks[indexBefore].safetyCeiling).toBe("write");
  });

  it("runNow caches the fired run's session for the open-session jump", async () => {
    await useScheduledStore.getState().load();
    await useScheduledStore.getState().runNow("memory-organizer");
    const { runsByTask, runningId } = useScheduledStore.getState();
    expect(runningId).toBeNull();
    expect(runsByTask["memory-organizer"]).toBeTruthy();
  });

  it("applyFired caches the session and stamps lastRun", async () => {
    await useScheduledStore.getState().load();
    useScheduledStore.getState().applyFired({
      id: 1,
      taskId: "memory-organizer",
      sessionId: "sess-123",
      firedMs: 42,
      status: "ok",
    });
    const { runsByTask, tasks } = useScheduledStore.getState();
    expect(runsByTask["memory-organizer"]).toBe("sess-123");
    expect(tasks.find((t) => t.id === "memory-organizer")?.lastRun).toBe(42);
  });

  it("applyChanged replaces the task list wholesale, preserving cached runs", async () => {
    await useScheduledStore.getState().load();
    useScheduledStore.setState({ runsByTask: { keep: "sess-keep" } });
    useScheduledStore.getState().applyChanged([
      {
        id: "only",
        name: "Only One",
        cron: "0 0 9 * * *",
        kind: { kind: "prompt", value: "x" },
        safetyCeiling: "read_only",
        cadenceLabel: "Daily at 9:00 AM",
        paused: false,
      },
    ]);
    const { tasks, runsByTask } = useScheduledStore.getState();
    expect(tasks).toHaveLength(1);
    expect(tasks[0].id).toBe("only");
    expect(runsByTask["keep"]).toBe("sess-keep");
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
