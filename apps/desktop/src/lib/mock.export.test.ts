import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { Message } from "../bindings";

/** Reach into the mock's private message map to seed a fixture message. */
function pushMessage(ipc: MockIpc, sessionId: string, m: Message) {
  (ipc as unknown as { messages: Map<string, Message[]> }).messages
    .get(sessionId)
    ?.push(m);
}

describe("MockIpc exportSession (#278)", () => {
  it("JSON export round-trips back to the session + messages", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession("ship the export feature");
    await ipc.sendMessage(session.id, "hello there");
    const before = await ipc.getMessages(session.id);

    const json = await ipc.exportSession(session.id, "json");
    const parsed = JSON.parse(json) as {
      session: { id: string; goal: string | null };
      messages: unknown[];
    };
    expect(parsed.session.id).toBe(session.id);
    expect(parsed.session.goal).toBe("ship the export feature");
    expect(parsed.messages).toEqual(before);
  });

  it("Markdown export has the title and role headings", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    await ipc.renameSession(session.id, "My Session");
    await ipc.sendMessage(session.id, "a question");

    const md = await ipc.exportSession(session.id, "markdown");
    expect(md).toContain("# My Session");
    expect(md).toContain("## You");
    expect(md).toContain("a question");
  });

  it("folds a long tool result in the Markdown export", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    const big = "x".repeat(5000);
    pushMessage(ipc, session.id, {
      id: "t1",
      sessionId: session.id,
      role: "tool",
      content: big,
      toolCallId: "c1",
      createdAt: Date.now(),
    });

    const md = await ipc.exportSession(session.id, "markdown");
    expect(md).toContain("more chars truncated");
    expect(md.length).toBeLessThan(big.length);
  });

  it("rejects an unknown session id", async () => {
    const ipc = new MockIpc();
    await expect(ipc.exportSession("nope", "json")).rejects.toThrow(/unknown/);
  });
});
