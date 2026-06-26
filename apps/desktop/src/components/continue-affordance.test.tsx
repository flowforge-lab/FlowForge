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
    cappedBySession: {},
    turnStartBySession: {},
    streamingBySession: {},
  });
});

describe("ContinueAffordance (#513)", () => {
  it("renders nothing when the session is not capped", () => {
    useChatStore.setState({ cappedBySession: {} });
    render(<ContinueAffordance sessionId={SID} />);
    expect(screen.queryByText("Continue")).toBeNull();
  });

  it("shows the button when the session is capped and idle", () => {
    useChatStore.setState({ cappedBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={SID} />);
    expect(screen.getByText("Continue")).not.toBeNull();
  });

  it("hides while a turn is pending or streaming, even if capped", () => {
    useChatStore.setState({
      cappedBySession: { [SID]: true },
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
    useChatStore.setState({ cappedBySession: { [SID]: true } });
    render(<ContinueAffordance sessionId={undefined} />);
    expect(screen.queryByText("Continue")).toBeNull();
  });

  it("sends 'continue' into the session on click", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    useChatStore.setState({ cappedBySession: { [SID]: true }, send });
    render(<ContinueAffordance sessionId={SID} />);
    await userEvent.click(screen.getByText("Continue"));
    expect(send).toHaveBeenCalledWith("continue", SID);
  });
});
