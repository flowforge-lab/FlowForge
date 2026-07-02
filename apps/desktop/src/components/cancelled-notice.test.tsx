// @vitest-environment jsdom

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  CancelledNotice,
  parseStopNotice,
} from "@/components/cancelled-notice";
import { useChatStore } from "@/store/chat";

const SID = "s-cancel";

afterEach(() => {
  cleanup();
  useChatStore.setState({
    resumableBySession: {},
    turnStartBySession: {},
    streamingBySession: {},
  });
});

describe("parseStopNotice (#636)", () => {
  it("labels a bare [stopped] as a cancel", () => {
    expect(parseStopNotice("[stopped]")).toEqual({
      label: "Cancelled",
      reason: "cancelled",
    });
    expect(parseStopNotice("  [stopped]\n")).toEqual({
      label: "Cancelled",
      reason: "cancelled",
    });
  });

  it("surfaces the reason text for a [stopped: …] cap/stall", () => {
    expect(parseStopNotice("[stopped: reached tool-call limit]")).toEqual({
      label: "reached tool-call limit",
      reason: "capped",
    });
  });
});

describe("CancelledNotice (#636)", () => {
  it("renders the Cancelled label + Continue when resumable and idle", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(<CancelledNotice sessionId={SID} content="[stopped]" />);
    expect(screen.getByText("Cancelled")).not.toBeNull();
    expect(screen.getByText("Continue")).not.toBeNull();
  });

  it("renders the stop reason for a capped notice", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(
      <CancelledNotice
        sessionId={SID}
        content="[stopped: reached tool-call limit]"
      />,
    );
    expect(screen.getByText("reached tool-call limit")).not.toBeNull();
  });

  it("shows the label but hides Continue while a turn is busy", () => {
    useChatStore.setState({
      resumableBySession: { [SID]: true },
      streamingBySession: { [SID]: "m1" },
    });
    render(<CancelledNotice sessionId={SID} content="[stopped]" />);
    expect(screen.getByText("Cancelled")).not.toBeNull();
    expect(screen.queryByText("Continue")).toBeNull();
  });
});
