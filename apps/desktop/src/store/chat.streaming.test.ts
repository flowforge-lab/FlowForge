import { beforeEach, describe, expect, it } from "vitest";

import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";

const SESSION = "s1";
const MESSAGE = "m1";
const CALL = "c1";

function seedRunningStep(partial: Partial<ToolStep> = {}): void {
  useChatStore.setState({
    toolStepsByMessage: {
      [MESSAGE]: [
        {
          callId: CALL,
          tool: "bash",
          args: { command: "cargo build" },
          status: "running",
          startedAt: Date.now(),
          ...partial,
        },
      ],
    },
  });
}

const stepOf = () => useChatStore.getState().toolStepsByMessage[MESSAGE]?.[0];

describe("chat store — live tool output streaming (#680)", () => {
  beforeEach(() => {
    useChatStore.setState({ toolStepsByMessage: {} });
  });

  it("accumulates output chunks in arrival order on the running step", () => {
    seedRunningStep();
    const chunk = (delta: string) =>
      useChatStore.getState().applyToolOutputChunk({
        sessionId: SESSION,
        messageId: MESSAGE,
        callId: CALL,
        stream: "stdout",
        delta,
      });

    chunk("Compiling ");
    chunk("ff-core\n");
    chunk("Finished\n");

    expect(stepOf()?.output).toBe("Compiling ff-core\nFinished\n");
    // Live stream is distinct from the (still-absent) final result.
    expect(stepOf()?.result).toBeUndefined();
  });

  it("interleaves stdout and stderr chunks in arrival order", () => {
    seedRunningStep();
    const emit = (stream: "stdout" | "stderr", delta: string) =>
      useChatStore.getState().applyToolOutputChunk({
        sessionId: SESSION,
        messageId: MESSAGE,
        callId: CALL,
        stream,
        delta,
      });

    emit("stdout", "out1\n");
    emit("stderr", "err1\n");
    emit("stdout", "out2\n");

    expect(stepOf()?.output).toBe("out1\nerr1\nout2\n");
  });

  it("final result supersedes the live output when the tool finishes", () => {
    seedRunningStep({ output: "partial tail..." });
    useChatStore.getState().applyToolResult({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      success: true,
      result: "exit_code: 0\n--- stdout ---\nfull capped output",
    });

    expect(stepOf()?.status).toBe("done");
    expect(stepOf()?.result).toBe(
      "exit_code: 0\n--- stdout ---\nfull capped output",
    );
    // The live output is left intact but the render prefers `result` once done.
    expect(stepOf()?.output).toBe("partial tail...");
  });

  it("drops a chunk with no matching running step (out-of-order / lost call)", () => {
    seedRunningStep();
    useChatStore.getState().applyToolOutputChunk({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: "unknown-call",
      stream: "stdout",
      delta: "orphan",
    });
    expect(stepOf()?.output).toBeUndefined();
  });
});
