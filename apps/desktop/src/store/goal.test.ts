// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import type { Goal } from "@/bindings/Goal";
import { useGoalStore } from "@/store/goal";

function goal(partial: Partial<Goal> & { sessionId: string }): Goal {
  return {
    objective: "ship it",
    status: "active",
    iteration: 0,
    budget: { maxIterations: 40 },
    spent: { iterations: 0 },
    ledger: [],
    createdMs: 0,
    updatedMs: 0,
    ...partial,
  } as unknown as Goal;
}

describe("useGoalStore.start", () => {
  beforeEach(() => {
    useGoalStore.setState({ bySession: {} });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("calls goalSet with the objective + max iterations and upserts the goal", async () => {
    const returned = goal({ sessionId: "s1", objective: "refactor auth" });
    const spy = vi.spyOn(ipc, "goalSet").mockResolvedValue(returned);

    await useGoalStore.getState().start("s1", "refactor auth", 12);

    expect(spy).toHaveBeenCalledWith(
      "s1",
      "refactor auth",
      12,
      undefined,
      undefined,
      undefined,
    );
    expect(useGoalStore.getState().bySession.s1).toEqual(returned);
  });

  it("forwards the propose_pr authorisation flag to goalSet", async () => {
    const spy = vi
      .spyOn(ipc, "goalSet")
      .mockResolvedValue(goal({ sessionId: "s3" }));

    await useGoalStore.getState().start("s3", "ship it", undefined, true);

    expect(spy).toHaveBeenCalledWith(
      "s3",
      "ship it",
      undefined,
      undefined,
      undefined,
      true,
    );
  });

  it("passes undefined max iterations so the backend applies its own default", async () => {
    const spy = vi
      .spyOn(ipc, "goalSet")
      .mockResolvedValue(goal({ sessionId: "s2" }));

    await useGoalStore.getState().start("s2", "do the thing", undefined);

    expect(spy).toHaveBeenCalledWith(
      "s2",
      "do the thing",
      undefined,
      undefined,
      undefined,
      undefined,
    );
  });

  it("does not upsert when the backend rejects the start", async () => {
    vi.spyOn(ipc, "goalSet").mockRejectedValue(
      new Error("loop already running"),
    );

    await expect(useGoalStore.getState().start("s3", "nope")).rejects.toThrow(
      "loop already running",
    );
    expect(useGoalStore.getState().bySession.s3).toBeUndefined();
  });
});
