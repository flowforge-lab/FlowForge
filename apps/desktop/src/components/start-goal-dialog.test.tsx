// @vitest-environment jsdom

import {
  render,
  screen,
  cleanup,
  fireEvent,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import type { Goal } from "@/bindings/Goal";
import { StartGoalDialog } from "@/components/start-goal-dialog";
import { useGoalDialogStore } from "@/store/goal-dialog";
import { useGoalStore } from "@/store/goal";

const SID = "s-goal";

function fakeGoal(): Goal {
  return {
    sessionId: SID,
    objective: "refactor auth",
    status: "active",
    iteration: 0,
    budget: { maxIterations: 40 },
    spent: { iterations: 0 },
    ledger: [],
    createdMs: 0,
    updatedMs: 0,
  } as unknown as Goal;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  useGoalDialogStore.setState({ sessionId: null });
  useGoalStore.setState({ bySession: {} });
});

describe("StartGoalDialog (#816)", () => {
  beforeEach(() => {
    useGoalDialogStore.getState().open(SID);
  });

  it("renders nothing when the dialog is closed", () => {
    useGoalDialogStore.getState().close();
    render(<StartGoalDialog />);
    expect(screen.queryByText("Start a goal")).toBeNull();
  });

  it("disables Start until the objective is non-blank", () => {
    render(<StartGoalDialog />);
    const start = screen.getByRole("button", {
      name: "Start goal",
    }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "   " },
    });
    expect(start.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "refactor auth" },
    });
    expect(start.disabled).toBe(false);
  });

  it("starts the goal with the trimmed objective + parsed iterations, then closes", async () => {
    const spy = vi.spyOn(ipc, "goalSet").mockResolvedValue(fakeGoal());
    render(<StartGoalDialog />);

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "  refactor auth  " },
    });
    fireEvent.change(screen.getByLabelText("Max iterations"), {
      target: { value: "12" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start goal" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("s-goal", "refactor auth", 12),
    );
    await waitFor(() =>
      expect(useGoalDialogStore.getState().sessionId).toBeNull(),
    );
  });

  it("passes undefined iterations when the field is blank", async () => {
    const spy = vi.spyOn(ipc, "goalSet").mockResolvedValue(fakeGoal());
    render(<StartGoalDialog />);

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "do the thing" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start goal" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("s-goal", "do the thing", undefined),
    );
  });

  it("keeps the dialog open when the backend rejects the start", async () => {
    vi.spyOn(ipc, "goalSet").mockRejectedValue(
      new Error("loop already running"),
    );
    render(<StartGoalDialog />);

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "refactor auth" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start goal" }));

    await waitFor(() =>
      expect(useGoalDialogStore.getState().sessionId).toBe(SID),
    );
  });
});
