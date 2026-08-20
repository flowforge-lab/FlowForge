import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { ToolStep } from "@/store/chat";

const SESSION = "s1";
const MESSAGE = "m1";
const CALL = "c1";
const TOOL = "edit";

function seedStep(partial: Partial<ToolStep> = {}): void {
  useChatStore.setState({
    toolStepsByMessage: {
      [MESSAGE]: [
        {
          callId: CALL,
          tool: TOOL,
          args: { path: "README.md" },
          status: "awaiting-approval",
          safety: "write",
          ...partial,
        },
      ],
    },
    sessionApprovedBySession: {},
    alwaysApproved: new Set<string>(),
  });
}

const stepOf = () => useChatStore.getState().toolStepsByMessage[MESSAGE]?.[0];

describe("chat store — four-option approval (#229)", () => {
  beforeEach(() => {
    useChatStore.setState({
      toolStepsByMessage: {},
      sessionApprovedBySession: {},
      alwaysApproved: new Set<string>(),
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("approveSession writes the session set, releases the call, and badges the step", async () => {
    const setSpy = vi.spyOn(ipc, "setSessionApprove").mockResolvedValue();
    const respondSpy = vi.spyOn(ipc, "respondApproval").mockResolvedValue();
    seedStep();

    await useChatStore.getState().approveSession(SESSION, MESSAGE, CALL, TOOL);

    expect(setSpy).toHaveBeenCalledWith(SESSION, TOOL);
    expect(respondSpy).toHaveBeenCalledWith(SESSION, CALL, true);
    expect(stepOf()?.status).toBe("running");
    expect(stepOf()?.sessionApproved).toBe(true);
    expect(
      useChatStore.getState().sessionApprovedBySession[SESSION]?.has(TOOL),
    ).toBe(true);
  });

  it("approveSession reverts the step and mirror when the ipc call fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(ipc, "setSessionApprove").mockRejectedValue(new Error("offline"));
    vi.spyOn(ipc, "respondApproval").mockResolvedValue();
    seedStep();

    await useChatStore.getState().approveSession(SESSION, MESSAGE, CALL, TOOL);

    expect(stepOf()?.status).toBe("awaiting-approval");
    expect(stepOf()?.sessionApproved).toBe(false);
    expect(
      useChatStore.getState().sessionApprovedBySession[SESSION]?.has(TOOL),
    ).toBe(false);
  });

  it("approveAlways persists the tool, releases the call, and badges the step", async () => {
    const setSpy = vi.spyOn(ipc, "setAlwaysApprove").mockResolvedValue();
    const respondSpy = vi.spyOn(ipc, "respondApproval").mockResolvedValue();
    seedStep();

    await useChatStore.getState().approveAlways(SESSION, MESSAGE, CALL, TOOL);

    expect(setSpy).toHaveBeenCalledWith(TOOL);
    expect(respondSpy).toHaveBeenCalledWith(SESSION, CALL, true);
    expect(stepOf()?.status).toBe("running");
    expect(stepOf()?.alwaysApproved).toBe(true);
    expect(useChatStore.getState().alwaysApproved.has(TOOL)).toBe(true);
  });

  it("loadAlwaysApproved hydrates the mirror from ipc", async () => {
    vi.spyOn(ipc, "listAlwaysApproved").mockResolvedValue([
      "read_file",
      "grep",
    ]);

    await useChatStore.getState().loadAlwaysApproved();

    expect(useChatStore.getState().alwaysApproved.has("read_file")).toBe(true);
    expect(useChatStore.getState().alwaysApproved.has("grep")).toBe(true);
  });

  it("revokeAlways removes the tool from the mirror via ipc", async () => {
    const spy = vi.spyOn(ipc, "removeAlwaysApprove").mockResolvedValue();
    useChatStore.setState({ alwaysApproved: new Set([TOOL]) });

    await useChatStore.getState().revokeAlways(TOOL);

    expect(spy).toHaveBeenCalledWith(TOOL);
    expect(useChatStore.getState().alwaysApproved.has(TOOL)).toBe(false);
  });

  it("revokeAlways restores the entry when the ipc call fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(ipc, "removeAlwaysApprove").mockRejectedValue(
      new Error("offline"),
    );
    useChatStore.setState({ alwaysApproved: new Set([TOOL]) });

    await useChatStore.getState().revokeAlways(TOOL);

    expect(useChatStore.getState().alwaysApproved.has(TOOL)).toBe(true);
  });

  it("approveAlways is a no-op for dangerous-safety tools (#232 allowlist exclusion)", async () => {
    const spy = vi.spyOn(ipc, "setAlwaysApprove").mockResolvedValue();
    seedStep({ safety: "dangerous", status: "awaiting-approval" });

    await useChatStore.getState().approveAlways(SESSION, MESSAGE, CALL, TOOL);

    expect(spy).not.toHaveBeenCalled();
    expect(useChatStore.getState().alwaysApproved.has(TOOL)).toBe(false);
    expect(stepOf()?.status).toBe("awaiting-approval");
    expect(stepOf()?.alwaysApproved).toBeUndefined();
  });

  it("approveSession is a no-op for dangerous-safety tools (#232 allowlist exclusion)", async () => {
    const spy = vi.spyOn(ipc, "setSessionApprove").mockResolvedValue();
    seedStep({ safety: "dangerous", status: "awaiting-approval" });

    await useChatStore.getState().approveSession(SESSION, MESSAGE, CALL, TOOL);

    expect(spy).not.toHaveBeenCalled();
    expect(
      useChatStore.getState().sessionApprovedBySession[SESSION]?.has(TOOL),
    ).not.toBe(true);
    expect(stepOf()?.status).toBe("awaiting-approval");
    expect(stepOf()?.sessionApproved).toBeUndefined();
  });

  it("applyToolCall pre-badges a follow-up call of an already-approved tool", () => {
    useChatStore.setState({
      alwaysApproved: new Set([TOOL]),
      sessionApprovedBySession: { [SESSION]: new Set(["grep"]) },
    });

    useChatStore.getState().applyToolCall({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: "c2",
      tool: TOOL,
      args: {},
    });
    expect(
      useChatStore.getState().toolStepsByMessage[MESSAGE]?.[0]?.alwaysApproved,
    ).toBe(true);

    useChatStore.getState().applyToolCall({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: "c3",
      tool: "grep",
      args: {},
    });
    const grepStep = useChatStore
      .getState()
      .toolStepsByMessage[MESSAGE]?.find((s) => s.callId === "c3");
    expect(grepStep?.sessionApproved).toBe(true);
  });
});

// #1270: the backend now expires an approval nobody answered and denies it. It
// emits no "prompt is dead" event — the resulting tool:result is what retires
// the prompt, exactly as a turn-cancel deny already does. That makes this the
// frontend half of the fix: if `applyToolResult` ever stopped overwriting an
// awaiting status, an expired prompt would sit there forever accepting clicks
// that go nowhere.
describe("chat store — an expired approval prompt is dismissed (#1270)", () => {
  beforeEach(() => {
    useChatStore.setState({
      toolStepsByMessage: {},
      sessionApprovedBySession: {},
      alwaysApproved: new Set<string>(),
    });
  });

  it("a denial result clears a step still awaiting approval", () => {
    seedStep();
    expect(stepOf()?.status).toBe("awaiting-approval");

    useChatStore.getState().applyToolResult({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      success: false,
      result:
        "call to `edit` was not run: the approval request expired with no answer",
    });

    expect(stepOf()?.status).toBe("error");
    expect(stepOf()?.result).toContain("expired with no answer");
  });

  it("a denial result clears a step still awaiting an ask_user answer", () => {
    seedStep({ status: "awaiting-answer", question: "still there?" });
    expect(stepOf()?.status).toBe("awaiting-answer");

    useChatStore.getState().applyToolResult({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      success: false,
      result: "[no answer: question dismissed]",
    });

    expect(stepOf()?.status).toBe("error");
  });
});

describe("chat store — propose_pr scope summary threading (#1252)", () => {
  beforeEach(() => {
    useChatStore.setState({
      toolStepsByMessage: {},
      sessionApprovedBySession: {},
      alwaysApproved: new Set<string>(),
    });
  });

  it("applyApprovalRequest carries scopeSummary onto the awaiting step", () => {
    useChatStore.setState({
      toolStepsByMessage: {
        [MESSAGE]: [
          { callId: CALL, tool: "propose_pr", args: {}, status: "running" },
        ],
      },
    });
    useChatStore.getState().applyApprovalRequest({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      tool: "propose_pr",
      args: {},
      safety: "publish",
      scopeSummary: {
        filesChanged: 2,
        insertions: 10,
        deletions: 3,
        newFiles: 1,
        perFile: [{ path: "a.ts", insertions: 10, deletions: 3 }],
      },
    });
    expect(stepOf()?.status).toBe("awaiting-approval");
    expect(stepOf()?.scopeSummary).toEqual({
      filesChanged: 2,
      insertions: 10,
      deletions: 3,
      newFiles: 1,
      perFile: [{ path: "a.ts", insertions: 10, deletions: 3 }],
    });
  });

  it("leaves scopeSummary undefined for a non-propose_pr gate", () => {
    useChatStore.setState({
      toolStepsByMessage: {
        [MESSAGE]: [
          { callId: CALL, tool: "edit", args: {}, status: "running" },
        ],
      },
    });
    useChatStore.getState().applyApprovalRequest({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: CALL,
      tool: "edit",
      args: {},
      safety: "write",
    });
    expect(stepOf()?.scopeSummary).toBeUndefined();
  });
});
