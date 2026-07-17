import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MockIpc } from "./mock";
import type { ProcessOutputEvent, ProcessExitedEvent } from "@/bindings";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("MockIpc background process (#987)", () => {
  it("streams process:output chunks and a terminal process:exited", async () => {
    const ipc = new MockIpc();
    const out: ProcessOutputEvent[] = [];
    const exits: ProcessExitedEvent[] = [];
    await ipc.onProcessOutput((e) => out.push(e));
    await ipc.onProcessExited((e) => exits.push(e));

    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "start the dev server");

    // Chunks are scheduled over time (a live stream), not emitted all at once.
    expect(out).toHaveLength(0);

    // Drain the whole schedule.
    await vi.runAllTimersAsync();

    expect(out.length).toBeGreaterThan(1);
    // All chunks belong to the same session + process id, streamed in order.
    const pid = out[0].processId;
    expect(out.every((e) => e.sessionId === s.id)).toBe(true);
    expect(out.every((e) => e.processId === pid)).toBe(true);

    // Exactly one terminal event, with a success status.
    expect(exits).toHaveLength(1);
    expect(exits[0].processId).toBe(pid);
    expect(exits[0].status).toBe("exited(0)");
  });

  it("keeps streaming across turns — output outlives the launching turn", async () => {
    const ipc = new MockIpc();
    const out: ProcessOutputEvent[] = [];
    await ipc.onProcessOutput((e) => out.push(e));

    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "start the dev server");

    // Advance only partway — some chunks have landed, but not the whole stream.
    await vi.advanceTimersByTimeAsync(1600);
    const afterTurnOne = out.length;
    expect(afterTurnOne).toBeGreaterThan(0);

    // A later turn happens; the same process is still streaming.
    await ipc.sendMessage(s.id, "what's the status?");
    await vi.advanceTimersByTimeAsync(3000);
    expect(out.length).toBeGreaterThan(afterTurnOne);
  });

  it("starts only one demo process per session, even across turns", async () => {
    const ipc = new MockIpc();
    const out: ProcessOutputEvent[] = [];
    await ipc.onProcessOutput((e) => out.push(e));

    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "first turn");
    await ipc.sendMessage(s.id, "second turn");
    await vi.runAllTimersAsync();

    // All output belongs to a single processId despite two turns.
    const ids = new Set(out.map((e) => e.processId));
    expect(ids.size).toBe(1);
  });
});
