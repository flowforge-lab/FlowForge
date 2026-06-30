// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { InputBar, ModePill } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";
import { useSessionModeStore } from "@/store/session-mode";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import type { Message } from "@/bindings";

// Radix DropdownMenu calls these pointer/scroll APIs that jsdom doesn't implement.
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
});

// The pill trigger's text (dot is a span, so textContent is just the mode label).
function pillLabel(testId: string): string {
  return (
    within(screen.getByTestId(testId)).getByRole("button").textContent ?? ""
  );
}

/** Open a pane's mode dropdown and select `mode` — the new one-click direct-select. */
async function selectMode(testId: string, mode: string) {
  const user = userEvent.setup();
  await user.click(within(screen.getByTestId(testId)).getByRole("button"));
  await user.click(
    await screen.findByRole("menuitem", { name: new RegExp(mode) }),
  );
}

describe("ModePill dropdown (#344)", () => {
  beforeEach(() => {
    localStorage.clear();
    usePrefsStore.setState({ defaultMode: "auto" });
    useSessionModeStore.setState({ modeBySession: {} });
    render(
      <>
        <div data-testid="a">
          <ModePill sessionId="a" />
        </div>
        <div data-testid="b">
          <ModePill sessionId="b" />
        </div>
      </>,
    );
  });

  afterEach(() => cleanup());

  it("shows the default mode and switches directly to the chosen mode in one step", async () => {
    expect(pillLabel("a")).toBe("Auto"); // inherits defaultMode

    await selectMode("a", "Plan");
    expect(pillLabel("a")).toBe("Plan");

    await selectMode("a", "Act");
    expect(pillLabel("a")).toBe("Act");
  });

  it("keeps each session's (pane's) mode independent", async () => {
    await selectMode("a", "Plan");
    expect(pillLabel("a")).toBe("Plan");
    expect(pillLabel("b")).toBe("Auto"); // b untouched
  });

  it("persists the per-session mode to localStorage", async () => {
    await selectMode("a", "Plan");
    const blob = JSON.parse(localStorage.getItem("ff-session-mode") ?? "{}");
    expect(blob.state.modeBySession.a).toBe("plan");
  });
});

// InputBar-level behavior tied to mode (placeholder + the removed handoff, #344).
const SID = "s1";

function assistantMsg(content = "Here is the plan…"): Message {
  return { id: "m1", sessionId: SID, role: "assistant", content, createdAt: 0 };
}

function seed(mode: "plan" | "act" | "auto", withReply = false) {
  usePrefsStore.setState({ defaultMode: "auto" });
  useSessionModeStore.setState({ modeBySession: { [SID]: mode } });
  useComposerStore.setState({
    textBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: withReply ? { [SID]: [assistantMsg()] } : {},
    streamingBySession: {},
    turnStartBySession: {},
  });
}

describe("InputBar mode behavior (#344)", () => {
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

  it("uses a Plan-aware composer placeholder", () => {
    seed("plan");
    render(<InputBar sessionId={SID} />);
    expect(
      screen.getByPlaceholderText(/plan mode — ask the agent to read/i),
    ).not.toBeNull();
  });

  it("does not render the old Plan→Act handoff button after a plan reply", () => {
    seed("plan", /* withReply */ true);
    render(<InputBar sessionId={SID} />);
    expect(
      screen.queryByRole("button", { name: /switch to act & continue/i }),
    ).toBeNull();
  });

  it("renders the workspace bar as a distinct element from the composer toolbar (#606)", () => {
    seed("act");
    // Seed the workspace synchronously so the folder chip renders without
    // waiting on the (mocked) async load.
    useSessionWorkspaceStore.setState({
      bySession: { [SID]: { path: "/home/me/flowforge", gitBranch: null } },
      recents: ["/home/me/flowforge"],
    });
    render(<InputBar sessionId={SID} />);

    const bar = screen.getByTestId("workspace-bar");
    // The workspace chip lives in the bar… (getByText throws if absent).
    within(bar).getByText("flowforge");
    // …and the Send button does NOT — it stays in the composer card's toolbar.
    const send = screen.getByRole("button", { name: /send/i });
    expect(within(bar).queryByRole("button", { name: /send/i })).toBeNull();
    expect(send.closest('[data-testid="workspace-bar"]')).toBeNull();
  });
});
