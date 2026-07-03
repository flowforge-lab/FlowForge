import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc skill telemetry (RFC 0001 §8)", () => {
  it("returns a stable aggregate for an installed skill", async () => {
    const ipc = new MockIpc();
    const agg = await ipc.getSkillTelemetry("rust-debugging");
    expect(agg).not.toBeNull();
    // Deterministic across calls.
    const again = await ipc.getSkillTelemetry("rust-debugging");
    expect(again).toEqual(agg);
    // Aggregate invariants the backend also upholds.
    expect(agg!.skill).toBe("rust-debugging");
    expect(agg!.successes).toBeLessThanOrEqual(agg!.completions);
    expect(agg!.successRate).toBeCloseTo(agg!.successes / agg!.completions);
    expect(agg!.activations).toBeGreaterThanOrEqual(agg!.completions);
  });

  it("returns null for an unknown skill", async () => {
    const ipc = new MockIpc();
    expect(await ipc.getSkillTelemetry("does-not-exist")).toBeNull();
  });
});

describe("MockIpc session:title-updated (#671 item 2b)", () => {
  it("fires once after the first turn with a summarized title", async () => {
    const ipc = new MockIpc();
    const events: { sessionId: string; title: string }[] = [];
    await ipc.onSessionTitleUpdated((e) => events.push(e));

    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "help me fix the flaky parser test");
    // The title is emitted on a deferred tick, mirroring the post-turn backend.
    await new Promise((r) => setTimeout(r, 0));

    expect(events).toHaveLength(1);
    expect(events[0].sessionId).toBe(s.id);
    expect(events[0].title.length).toBeGreaterThan(0);

    // A second message (past the first turn) does not re-title.
    await ipc.sendMessage(s.id, "now check the linter");
    await new Promise((r) => setTimeout(r, 0));
    expect(events).toHaveLength(1);
  });
});
