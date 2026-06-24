// @vitest-environment jsdom

import { render, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore, type ToolStep } from "@/store/chat";
import {
  useExperimentalStore,
  EXPERIMENTAL_DEFAULTS,
} from "@/store/experimental";
import type { Message } from "@/bindings";

const SID = "s1";

const STEPS: ToolStep[] = [
  {
    callId: "c1",
    tool: "bash",
    args: { command: "ls" },
    status: "done",
    startedAt: 1000,
    finishedAt: 1500,
    result: "out",
  },
  {
    callId: "c2",
    tool: "grep",
    args: {},
    status: "done",
    startedAt: 1500,
    finishedAt: 4000,
    result: "x",
  },
];

function seed() {
  const msgs: Message[] = [
    { id: "u1", sessionId: SID, role: "user", content: "go", createdAt: 1 },
    {
      id: "a1",
      sessionId: SID,
      role: "assistant",
      content: "done",
      createdAt: 1,
    },
  ];
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: msgs },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: { a1: STEPS },
    reasoningByMessage: {},
  });
}

afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useExperimentalStore.setState({ flags: { ...EXPERIMENTAL_DEFAULTS } });
});

describe("ChatView step-timeline export affordance (#417)", () => {
  it("hides the download control when the experimental flag is off", () => {
    useExperimentalStore.setState({ flags: { ...EXPERIMENTAL_DEFAULTS } });
    seed();
    const { container } = render(<ChatView />);
    expect(
      within(container).queryByLabelText("Export step timeline"),
    ).toBeNull();
  });

  it("shows the download control when the flag is on", () => {
    useExperimentalStore.setState({
      flags: { ...EXPERIMENTAL_DEFAULTS, stepTimelineExport: true },
    });
    seed();
    const { container } = render(<ChatView />);
    expect(
      within(container).getByLabelText("Export step timeline"),
    ).toBeTruthy();
  });
});
