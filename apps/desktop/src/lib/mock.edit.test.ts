import { afterEach, describe, expect, it, vi } from "vitest";

import { MockIpc } from "./mock";

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("MockIpc editMessage (#463)", () => {
  it("replaces the user message in place and truncates the prior turn", async () => {
    // Freeze the assistant stream so the transcript is deterministic.
    vi.useFakeTimers();
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "first prompt");

    const before = await ipc.getMessages(s.id);
    const userMsg = before.find((m) => m.role === "user")!;
    expect(userMsg.content).toBe("first prompt");
    // The first turn produced at least one assistant message synchronously.
    expect(before.some((m) => m.role === "assistant")).toBe(true);

    const editedId = await ipc.editMessage(s.id, userMsg.id, "edited prompt");
    expect(editedId).toBe(userMsg.id); // edited in place, same id

    const after = await ipc.getMessages(s.id);
    const users = after.filter((m) => m.role === "user");
    // Still exactly one user message — replaced, not appended.
    expect(users).toHaveLength(1);
    expect(users[0].content).toBe("edited prompt");
    // The old assistant response was truncated; the re-run starts fresh after it.
    expect(after[0].id).toBe(userMsg.id);
  });

  it("rejects editing an unknown message", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await expect(ipc.editMessage(s.id, "ghost", "x")).rejects.toThrow(
      "no such message",
    );
  });

  it("rejects editing a non-user (assistant) message", async () => {
    vi.useFakeTimers();
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await ipc.sendMessage(s.id, "hi");
    const assistant = (await ipc.getMessages(s.id)).find(
      (m) => m.role === "assistant",
    )!;
    await expect(ipc.editMessage(s.id, assistant.id, "nope")).rejects.toThrow(
      "not a user message",
    );
  });
});
