// @vitest-environment jsdom

import { Profiler } from "react";
import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { SessionSidebar } from "@/components/session-sidebar";
import { useChatStore } from "@/store/chat";
import { useSessionPrefsStore } from "@/store/session-prefs";
import { usePrefsStore } from "@/store/prefs";
import type { Session } from "@/bindings";

// The sidebar renders a row per session, so a background session's token storm used to
// re-render the whole list once per delta (#1122) — competing with the foreground pane
// for the main thread. Companion to chat-view.isolation.test.tsx: same Profiler
// render-count shape, but driving the REAL applyToken so the store fix and the
// subscription fix are tested together.

const A = "sess-a";
const B = "sess-b";

function session(id: string): Session {
  return {
    id,
    goal: `Session ${id}`,
    title: id,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

function seed() {
  localStorage.clear();
  usePrefsStore.setState({ sidebarCollapsed: false });
  useSessionPrefsStore.setState({ pinned: [], dismissed: [] });
  useChatStore.setState({
    sessions: [session(A), session(B)],
    activeSessionId: A,
    draftSessionIds: new Set(),
    messagesBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    recentlyFinishedBySession: {},
  });
}

function renderSidebar() {
  let renders = 0;
  render(
    <Profiler id="sidebar" onRender={() => (renders += 1)}>
      <SessionSidebar />
    </Profiler>,
  );
  return () => renders;
}

const token = (sessionId: string, messageId: string, delta: string) =>
  act(() => {
    useChatStore.getState().applyToken({ sessionId, messageId, delta });
  });

describe("SessionSidebar streaming isolation (#1122)", () => {
  beforeEach(seed);
  afterEach(() => {
    useChatStore.setState({ sessions: [], messagesBySession: {} });
    localStorage.clear();
  });

  it("does not re-render per token while a background session streams", () => {
    const renders = renderSidebar();

    // Token 1 legitimately re-renders: session B enters the streaming state, which
    // is exactly what the row's spinner is for.
    token(B, "b-a1", "He");
    const afterFirstToken = renders();

    // Every later token of the same turn changes no value the sidebar renders.
    token(B, "b-a1", "llo");
    token(B, "b-a1", " world");
    token(B, "b-a1", "!");

    expect(renders()).toBe(afterFirstToken);
  });

  it("re-renders when another session enters streaming (positive control)", () => {
    const renders = renderSidebar();
    token(B, "b-a1", "hi");
    const before = renders();

    token(A, "a-a1", "hi");

    expect(renders()).toBeGreaterThan(before);
  });

  it("re-renders when a session's finished flag flips (positive control)", () => {
    const renders = renderSidebar();
    token(B, "b-a1", "hi");
    const before = renders();

    act(() => {
      useChatStore.setState((s) => ({
        recentlyFinishedBySession: {
          ...s.recentlyFinishedBySession,
          [B]: Date.now(),
        },
      }));
    });

    expect(renders()).toBeGreaterThan(before);
  });
});
