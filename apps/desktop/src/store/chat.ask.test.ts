import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";

const SESSION = "s1";
const MESSAGE = "m1";
const CALL = "c1";
const QUESTION = "Which file should I update?";

function seedStep(partial: Partial<ToolStep> = {}): void {
  useChatStore.setState({
    toolStepsByMessage: {
      [MESSAGE]: [
        {
          callId: CALL,
          tool: "ask_user",
          args: { question: QUESTION },
          status: "running",
          ...partial,
        },
      ],
    },
  });
}

const stepOf = (messageId = MESSAGE) =>
  useChatStore.getState().toolStepsByMessage[messageId]?.[0];

describe("chat store — ask_user (#44)", () => {
  beforeEach(() => {
    useChatStore.setState({ toolStepsByMessage: {} });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("applyAskRequest flips the matching step to awaiting-answer and stores the question", () => {
    seedStep();
    useChatStore.getState().applyAskRequest({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      question: QUESTION,
      secret: false,
    });
    expect(stepOf()?.status).toBe("awaiting-answer");
    expect(stepOf()?.question).toBe(QUESTION);
  });

  it("applyAskRequest carries the secret flag onto the step (#562)", () => {
    seedStep();
    useChatStore.getState().applyAskRequest({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      question: "Enter your sudo password:",
      secret: true,
    });
    expect(stepOf()?.status).toBe("awaiting-answer");
    expect(stepOf()?.secret).toBe(true);
  });

  it("respondAsk never stashes the entered answer on the step (#562)", async () => {
    vi.spyOn(ipc, "respondAsk").mockResolvedValue();
    seedStep({ status: "awaiting-answer", question: QUESTION, secret: true });

    await useChatStore.getState().respondAsk(SESSION, MESSAGE, CALL, "hunter2");

    // The value goes straight to the turn via ipc — it must not be persisted on
    // the step (no cleartext secret in store state).
    const step = stepOf();
    expect(step?.status).toBe("running");
    expect(JSON.stringify(step)).not.toContain("hunter2");
  });

  it("applyToolResult on a secret step carries no cleartext — only the redacted placeholder (#562)", () => {
    // The backend redacts a secret answer at the source, so the returning
    // `tool:result` carries the placeholder — the actual round-trip leak vector.
    seedStep({ status: "running", question: QUESTION, secret: true });

    useChatStore.getState().applyToolResult({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      success: true,
      result: "[secret provided by user]",
    });

    const step = stepOf();
    expect(step?.status).toBe("done");
    expect(step?.result).toBe("[secret provided by user]");
    // Nothing cleartext lands on the step for the UI to render or retain.
    expect(JSON.stringify(step)).not.toContain("hunter2");
  });

  it("applyAskRequest is a no-op when the message has no tracked steps", () => {
    useChatStore.getState().applyAskRequest({
      sessionId: SESSION,
      messageId: "unknown",
      callId: CALL,
      question: QUESTION,
      secret: false,
    });
    expect(
      useChatStore.getState().toolStepsByMessage["unknown"],
    ).toBeUndefined();
  });

  it("respondAsk submits the answer through ipc and optimistically flips to running", async () => {
    const spy = vi.spyOn(ipc, "respondAsk").mockResolvedValue();
    seedStep({ status: "awaiting-answer", question: QUESTION });

    await useChatStore
      .getState()
      .respondAsk(SESSION, MESSAGE, CALL, "src/main.ts");

    expect(spy).toHaveBeenCalledWith(SESSION, CALL, "src/main.ts");
    expect(stepOf()?.status).toBe("running");
  });

  it("respondAsk reverts to the prompt (question intact) when the ipc call fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(ipc, "respondAsk").mockRejectedValue(new Error("offline"));
    seedStep({ status: "awaiting-answer", question: QUESTION });

    await useChatStore
      .getState()
      .respondAsk(SESSION, MESSAGE, CALL, "src/main.ts");

    expect(stepOf()?.status).toBe("awaiting-answer");
    expect(stepOf()?.question).toBe(QUESTION);
  });
});
