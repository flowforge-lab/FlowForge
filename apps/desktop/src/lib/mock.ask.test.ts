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

  it("a secret/password message drives the masked ask variant (#562)", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    const asks: { callId: string; secret: boolean }[] = [];
    await ipc.onAskRequest((e) => {
      asks.push({ callId: e.callId, secret: e.secret });
    });

    await ipc.sendMessage(session.id, "here is my sudo password");
    expect(asks).toHaveLength(1);
    expect(asks[0].secret).toBe(true);
  });

  it("a secret answer round-trips as the redacted placeholder, not cleartext (#562)", async () => {
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

    await ipc.sendMessage(session.id, "enter your password");
    await ipc.respondAsk(session.id, askCallId, "hunter2");

    const answered = results.filter((r) => r.callId === askCallId);
    expect(answered).toHaveLength(1);
    // The typed secret must never come back over the result event.
    expect(answered[0].result).toBe("[secret provided by user]");
    expect(answered[0].result).not.toContain("hunter2");
  });

  it("a plain message keeps the ask non-secret (#562)", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();

    const asks: { secret: boolean }[] = [];
    await ipc.onAskRequest((e) => {
      asks.push({ secret: e.secret });
    });

    await ipc.sendMessage(session.id, "hello");
    expect(asks).toHaveLength(1);
    expect(asks[0].secret).toBe(false);
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
