// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputBar } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";
import { useSessionModeStore } from "@/store/session-mode";
import type { Message } from "@/bindings";

const SID = "s1";

function assistantMsg(content = "Here is the plan…"): Message {
  return { id: "m1", sessionId: SID, role: "assistant", content, createdAt: 0 };
}

function seed(opts: {
  mode: "plan" | "act" | "auto";
  /** The session's last message: a non-empty assistant reply, an empty assistant
   *  stub, or none. */
  last?: "assistant" | "empty-assistant" | "none";
}) {
  usePrefsStore.setState({ defaultMode: "auto" });
  useSessionModeStore.setState({ modeBySession: { [SID]: opts.mode } });
  useComposerStore.setState({
    textBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  const messages: Record<string, Message[]> =
    opts.last === "assistant"
      ? { [SID]: [assistantMsg()] }
      : opts.last === "empty-assistant"
        ? { [SID]: [assistantMsg("   ")] }
        : {};
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: messages,
    streamingBySession: {},
    turnStartBySession: {},
  });
}

const handoff = () =>
  screen.queryByRole("button", { name: /switch to act & continue/i });

describe("Plan → Act handoff (#267)", () => {
  beforeEach(() => {
    vi.spyOn(ipc, "getSessionWorkspace").mockResolvedValue({
      path: "/tmp",
      gitBranch: null,
    });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("shows the handoff only in Plan after a non-empty assistant reply", () => {
    seed({ mode: "plan", last: "assistant" });
    const { unmount } = render(<InputBar sessionId={SID} />);
    expect(handoff()).not.toBeNull();
    unmount();

    seed({ mode: "act", last: "assistant" }); // not Plan
    render(<InputBar sessionId={SID} />);
    expect(handoff()).toBeNull();
  });

  it("hides the handoff in Plan when the agent hasn't replied yet", () => {
    seed({ mode: "plan", last: "none" });
    render(<InputBar sessionId={SID} />);
    expect(handoff()).toBeNull();
  });

  it("hides the handoff on an empty / tool-only assistant stub (#288)", () => {
    seed({ mode: "plan", last: "empty-assistant" });
    render(<InputBar sessionId={SID} />);
    expect(handoff()).toBeNull();
  });

  it("switching flips the session to Act and sends a continuation", () => {
    const sendSpy = vi.spyOn(ipc, "sendMessage").mockResolvedValue("m2");
    seed({ mode: "plan", last: "assistant" });
    render(<InputBar sessionId={SID} />);

    fireEvent.click(handoff()!);

    expect(useSessionModeStore.getState().modeBySession[SID]).toBe("act");
    expect(sendSpy).toHaveBeenCalledWith(SID, "Go ahead.");
    // TODO(#288): once the mode-IPC seam lands, also spy on `set_session_mode`
    // and assert it is awaited BEFORE `sendMessage`, so the continuation turn
    // can't start while the backend is still in Plan.
  });

  it("uses a Plan-aware composer placeholder", () => {
    seed({ mode: "plan", last: "none" });
    render(<InputBar sessionId={SID} />);
    expect(
      screen.getByPlaceholderText(/plan mode — ask the agent to read/i),
    ).not.toBeNull();
  });
});
