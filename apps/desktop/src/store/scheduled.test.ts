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
      historyByTask: {},
      loadingRunsIds: new Set(),
      pausedAll: false,
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
        catchUp: false,
      },
    ]);
    const { tasks, runsByTask } = useScheduledStore.getState();
    expect(tasks).toHaveLength(1);
    expect(tasks[0].id).toBe("only");
    expect(runsByTask["keep"]).toBe("sess-keep");
  });

  it("loadRuns caches a task's fire history newest-first", async () => {
    await useScheduledStore.getState().load();
    // Seed history by firing the task a couple of times through the mock.
    await useScheduledStore.getState().runNow("memory-organizer");
    await useScheduledStore.getState().runNow("memory-organizer");

    await useScheduledStore.getState().loadRuns("memory-organizer");
    const history =
      useScheduledStore.getState().historyByTask["memory-organizer"];
    expect(history?.length).toBeGreaterThanOrEqual(2);
    // Newest first: firedMs is non-increasing down the list.
    for (let i = 1; i < history.length; i += 1) {
      expect(history[i - 1].firedMs).toBeGreaterThanOrEqual(history[i].firedMs);
    }
    expect(
      useScheduledStore.getState().loadingRunsIds.has("memory-organizer"),
    ).toBe(false);
  });

  it("loadRuns tracks loading per task so panels are independent", async () => {
    await useScheduledStore.getState().load();
    // Two concurrent loads: each task ends up with its own history, and neither
    // completion leaves the other's loading flag stuck (C1 — no shared slot).
    await Promise.all([
      useScheduledStore.getState().loadRuns("memory-organizer"),
      useScheduledStore.getState().loadRuns("weekly-digest"),
    ]);
    const { historyByTask, loadingRunsIds } = useScheduledStore.getState();
    expect("memory-organizer" in historyByTask).toBe(true);
    expect("weekly-digest" in historyByTask).toBe(true);
    expect(loadingRunsIds.size).toBe(0);
  });

  it("loadRuns skips a second in-flight fetch for the same task", async () => {
    await useScheduledStore.getState().load();
    const first = useScheduledStore.getState().loadRuns("memory-organizer");
    // A second call while the first is in flight is a no-op (guards the same-task
    // out-of-order overwrite); it resolves immediately without a second fetch.
    await useScheduledStore.getState().loadRuns("memory-organizer");
    await first;
    expect(
      useScheduledStore.getState().loadingRunsIds.has("memory-organizer"),
    ).toBe(false);
  });

  it("applyFired prepends to an already-loaded history panel", async () => {
    await useScheduledStore.getState().load();
    useScheduledStore.setState({ historyByTask: { "memory-organizer": [] } });
    useScheduledStore.getState().applyFired({
      id: 7,
      taskId: "memory-organizer",
      sessionId: "sess-new",
      firedMs: 99,
      status: "ok",
    });
    const history =
      useScheduledStore.getState().historyByTask["memory-organizer"];
    expect(history).toHaveLength(1);
    expect(history[0].id).toBe(7);
  });

  it("applyFired does not create a history panel that was never opened", () => {
    useScheduledStore.getState().applyFired({
      id: 1,
      taskId: "never-opened",
      firedMs: 1,
      status: "ok",
    });
    expect("never-opened" in useScheduledStore.getState().historyByTask).toBe(
      false,
    );
  });

  it("setPausedAll engages the global kill-switch", async () => {
    await useScheduledStore.getState().setPausedAll(true);
    expect(useScheduledStore.getState().pausedAll).toBe(true);
    await useScheduledStore.getState().setPausedAll(false);
    expect(useScheduledStore.getState().pausedAll).toBe(false);
  });

  it("runNow surfaces an error and clears running when paused-all is engaged", async () => {
    await useScheduledStore.getState().load();
    await useScheduledStore.getState().setPausedAll(true);
    await useScheduledStore.getState().runNow("memory-organizer");
    expect(useScheduledStore.getState().error).toBeTruthy();
    expect(useScheduledStore.getState().runningId).toBeNull();
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
