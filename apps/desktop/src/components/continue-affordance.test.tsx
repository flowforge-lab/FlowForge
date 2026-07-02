// @vitest-environment jsdom

import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ContinueAffordance } from "@/components/continue-affordance";
import { useChatStore } from "@/store/chat";

const SID = "s-cap";

afterEach(() => {
  cleanup();
  useChatStore.setState({
    resumableBySession: {},
    turnStartBySession: {},
    streamingBySession: {},
  });
});

describe("ContinueAffordance (#513/#636)", () => {
  it("renders nothing when the session is not resumable", () => {
    useChatStore.setState({ resumableBySession: {} });
    render(<ContinueAffordance sessionId={SID} />);
    expect(screen.queryByText("Continue")).toBeNull();
  });

  it("shows the button when the session is resumable and idle", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={SID} />);
    expect(screen.getByText("Continue")).not.toBeNull();
  });

  it("hides while a turn is pending or streaming, even if resumable", () => {
    useChatStore.setState({
      resumableBySession: { [SID]: true },
      turnStartBySession: { [SID]: Date.now() },
    });
    const { rerender } = render(<ContinueAffordance sessionId={SID} />);
    expect(screen.queryByText("Continue")).toBeNull();

    useChatStore.setState({
      turnStartBySession: {},
      streamingBySession: { [SID]: "m1" },
    });
    rerender(<ContinueAffordance sessionId={SID} />);
    expect(screen.queryByText("Continue")).toBeNull();
  });

  it("renders nothing without a session id", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={undefined} />);
    expect(screen.queryByText("Continue")).toBeNull();
  });

  it("uses cap/stall tooltip copy by default", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={SID} />);
    expect(screen.getByRole("button").getAttribute("title")).toContain(
      "tool-call limit",
    );
  });

  it("uses cancel-specific tooltip copy when reason='cancelled' (#636)", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={SID} reason="cancelled" />);
    expect(screen.getByRole("button").getAttribute("title")).toBe(
      "Resume — you stopped this turn",
    );
  });

  it("sends 'continue' into the session on click", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    useChatStore.setState({ resumableBySession: { [SID]: true }, send });
    render(<ContinueAffordance sessionId={SID} />);
    await userEvent.click(screen.getByText("Continue"));
    expect(send).toHaveBeenCalledWith("continue", SID);
  });
});
