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
