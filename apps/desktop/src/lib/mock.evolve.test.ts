import { describe, expect, it } from "vitest";

import type { SkillEvolveApprovalRequestEvent } from "../bindings";
import { MockIpc } from "./mock";

describe("MockIpc skill optimize round-trip (Issue #29)", () => {
  it("optimizeSkill emits an approval request and bumps the version on approval", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    let req: SkillEvolveApprovalRequestEvent | null = null;
    await ipc.onEvolveApprovalRequest((e) => {
      req = e;
    });

    const promise = ipc.optimizeSkill(session.id, "rust-debugging");
    // Let the microtask emit the event.
    await Promise.resolve();
    expect(req).not.toBeNull();
    const event = req as unknown as SkillEvolveApprovalRequestEvent;
    expect(event.skill).toBe("rust-debugging");
    expect(event.currentVersion).toBe("0.1.0");
    expect(event.newVersion).toBe("0.1.1");
    expect(event.beforeBody).not.toBe(event.afterBody);
    expect(event.costEstimate.estimatedMeanTokens).toBeLessThan(
      event.costEstimate.currentMeanTokens,
    );

    // Approve via the reused respond_approval path (keyed by requestId).
    await ipc.respondApproval(event.requestId, event.requestId, true);
    const newVersion = await promise;
    expect(newVersion).toBe("0.1.1");

    const versions = await ipc.listSkillVersions("rust-debugging");
    expect(versions).toEqual(["0.1.0"]);
  });

  it("optimizeSkill rejects when the proposal is declined", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    let requestId = "";
    await ipc.onEvolveApprovalRequest((e) => {
      requestId = e.requestId;
    });

    const promise = ipc.optimizeSkill(session.id, "create-pr");
    await Promise.resolve();
    expect(requestId).not.toBe("");

    await ipc.respondApproval(requestId, requestId, false);
    await expect(promise).rejects.toThrow(/declined/);
    expect(await ipc.listSkillVersions("create-pr")).toEqual([]);
  });

  it("optimizeSkill rejects an unknown skill", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    await expect(ipc.optimizeSkill(session.id, "nope")).rejects.toThrow(
      /unknown skill/,
    );
  });

  it("rollbackSkill restores an archived version and archives the current one", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    let requestId = "";
    await ipc.onEvolveApprovalRequest((e) => {
      requestId = e.requestId;
    });

    const promise = ipc.optimizeSkill(session.id, "write-tests");
    await Promise.resolve();
    await ipc.respondApproval(requestId, requestId, true);
    expect(await promise).toBe("0.1.1");

    // Roll back to the archived 0.1.0 — the current 0.1.1 is archived in turn.
    await ipc.rollbackSkill("write-tests", "0.1.0");
    expect(await ipc.listSkillVersions("write-tests")).toEqual(["0.1.1"]);
  });

  it("rollbackSkill rejects an unknown version", async () => {
    const ipc = new MockIpc();
    await expect(ipc.rollbackSkill("write-tests", "9.9.9")).rejects.toThrow(
      /version not found/,
    );
  });
});
