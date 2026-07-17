import { afterEach, describe, expect, it } from "vitest";
import { useProcessesStore } from "@/store/processes";
import type { ProcessOutputEvent, ProcessExitedEvent } from "@/bindings";

function out(
  sessionId: string,
  processId: number,
  delta: string,
  stream: "stdout" | "stderr" = "stdout",
): ProcessOutputEvent {
  return { sessionId, processId, stream, delta };
}

function exited(
  sessionId: string,
  processId: number,
  status: string,
): ProcessExitedEvent {
  return { sessionId, processId, status };
}

afterEach(() => {
  useProcessesStore.setState({ bySession: {} });
});

describe("useProcessesStore (#987)", () => {
  it("materializes a process on its first chunk and appends in order", () => {
    const s = useProcessesStore.getState();
    s.applyProcessOutput(out("sess-a", 1, "hello "));
    s.applyProcessOutput(out("sess-a", 1, "world"));

    const p = useProcessesStore.getState().bySession["sess-a"]![1]!;
    expect(p.output).toBe("hello world");
    expect(p.status).toBeNull();
    expect(p.processId).toBe(1);
  });

  it("keeps distinct processIds independent within a session", () => {
    const s = useProcessesStore.getState();
    s.applyProcessOutput(out("sess-a", 1, "one"));
    s.applyProcessOutput(out("sess-a", 2, "two"));

    const byId = useProcessesStore.getState().bySession["sess-a"]!;
    expect(byId[1]!.output).toBe("one");
    expect(byId[2]!.output).toBe("two");
  });

  it("keeps sessions isolated", () => {
    const s = useProcessesStore.getState();
    s.applyProcessOutput(out("sess-a", 1, "a-out"));
    s.applyProcessOutput(out("sess-b", 1, "b-out"));

    expect(useProcessesStore.getState().bySession["sess-a"]![1]!.output).toBe(
      "a-out",
    );
    expect(useProcessesStore.getState().bySession["sess-b"]![1]!.output).toBe(
      "b-out",
    );
  });

  it("flips a process to its terminal status on exit, preserving output", () => {
    const s = useProcessesStore.getState();
    s.applyProcessOutput(out("sess-a", 1, "done\n"));
    s.applyProcessExited(exited("sess-a", 1, "exited(0)"));

    const p = useProcessesStore.getState().bySession["sess-a"]![1]!;
    expect(p.status).toBe("exited(0)");
    expect(p.output).toBe("done\n");
  });

  it("materializes an empty-output process if the exit arrives before any output", () => {
    const s = useProcessesStore.getState();
    s.applyProcessExited(exited("sess-a", 7, "failed: spawn error"));

    const p = useProcessesStore.getState().bySession["sess-a"]![7]!;
    expect(p.output).toBe("");
    expect(p.status).toBe("failed: spawn error");
  });

  it("accumulates across turns — output has no message/turn affinity", () => {
    const s = useProcessesStore.getState();
    // Turn N: first chunks.
    s.applyProcessOutput(out("sess-a", 1, "line1\n"));
    // ...an unrelated turn happens (no store interaction needed — the sink is
    // session-scoped, not turn-scoped) ... turn N+2: more chunks for the same
    // still-running process.
    s.applyProcessOutput(out("sess-a", 1, "line2\n"));

    const p = useProcessesStore.getState().bySession["sess-a"]![1]!;
    expect(p.output).toBe("line1\nline2\n");
    expect(p.status).toBeNull();
  });

  it("clear(sessionId) drops only that session's processes", () => {
    const s = useProcessesStore.getState();
    s.applyProcessOutput(out("sess-a", 1, "a"));
    s.applyProcessOutput(out("sess-b", 1, "b"));

    s.clear("sess-a");
    expect(useProcessesStore.getState().bySession["sess-a"]).toBeUndefined();
    expect(useProcessesStore.getState().bySession["sess-b"]).toBeDefined();
  });
});
