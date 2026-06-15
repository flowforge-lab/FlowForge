import { describe, expect, it } from "vitest";

import type { ToolResultEvent } from "../bindings";
import { MockIpc } from "./mock";

describe("MockIpc ask_user round-trip", () => {
  it("respondAsk resumes the turn with the answer as the tool result", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    let askCallId = "";
    await ipc.onAskRequest((e) => {
      askCallId = e.callId;
    });

    const results: ToolResultEvent[] = [];
    await ipc.onToolResult((e) => {
      results.push(e);
    });

    await ipc.sendMessage(session.id, "hello");
    expect(askCallId).not.toBe("");

    await ipc.respondAsk(session.id, askCallId, "src/main.ts");

    const answered = results.filter((r) => r.callId === askCallId);
    expect(answered).toHaveLength(1);
    expect(answered[0].success).toBe(true);
    expect(answered[0].result).toBe("src/main.ts");
  });
});

describe("MockIpc ask_user cancellation", () => {
  it("cancel during ask_user makes a later respondAsk a no-op (no duplicate tool:result)", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    let askCallId = "";
    await ipc.onAskRequest((e) => {
      askCallId = e.callId;
    });

    const results: ToolResultEvent[] = [];
    await ipc.onToolResult((e) => {
      results.push(e);
    });

    // The mock turn pauses at the interactive ask_user step.
    await ipc.sendMessage(session.id, "hello");
    expect(askCallId).not.toBe("");
    expect(results.some((r) => r.callId === askCallId)).toBe(false);

    // Cancel the turn while the ask is still pending — the backfill emits a
    // single "[cancelled]" result and must drop the pending resume.
    await ipc.cancelTurn(session.id);
    const cancelled = results.filter((r) => r.callId === askCallId);
    expect(cancelled).toHaveLength(1);
    expect(cancelled[0].success).toBe(false);
    expect(cancelled[0].result).toBe("[cancelled]");

    // A late answer must not resurrect the cancelled ask: no duplicate result.
    await ipc.respondAsk(session.id, askCallId, "src/main.ts");
    expect(results.filter((r) => r.callId === askCallId)).toHaveLength(1);
  });
});
