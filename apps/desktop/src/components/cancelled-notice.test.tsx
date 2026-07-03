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

  // #658: the structured stopReason is the primary path -- it wins over the
  // marker string, and works even when `content` carries no `[stopped…]` marker.
  it("prefers the structured stopReason over the marker text (#658)", () => {
    expect(parseStopNotice("[stopped]", "toolLimit")).toEqual({
      label: "reached tool-call limit",
      reason: "capped",
    });
    expect(parseStopNotice("anything", "cancelled")).toEqual({
      label: "Cancelled",
      reason: "cancelled",
    });
    expect(parseStopNotice("", "stall")).toEqual({
      label: "repeated a tool call without making progress",
      reason: "capped",
    });
    expect(parseStopNotice("", "emptyResponse")).toEqual({
      label: "the model returned an empty response",
      reason: "capped",
    });
    // #666: the drop-path interrupted reason resolves structurally, matching the
    // legacy `[stopped: interrupted]` fallback (resumable via the "capped" reason).
    expect(parseStopNotice("", "interrupted")).toEqual({
      label: "Interrupted",
      reason: "capped",
    });
  });

  it("falls back to the marker string when stopReason is absent (legacy row)", () => {
    expect(parseStopNotice("[stopped: reached tool-call limit]", null)).toEqual(
      {
        label: "reached tool-call limit",
        reason: "capped",
      },
    );
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

  it("renders from the structured stopReason with no marker in content (#658)", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(
      <CancelledNotice sessionId={SID} content="" stopReason="toolLimit" />,
    );
    expect(screen.getByText("reached tool-call limit")).not.toBeNull();
    expect(screen.getByText("Continue")).not.toBeNull();
  });

  it("renders the Interrupted label + Continue from the drop-path reason (#666)", () => {
    useChatStore.setState({ resumableBySession: { [SID]: true } });
    render(
      <CancelledNotice
        sessionId={SID}
        content="[stopped: interrupted]"
        stopReason="interrupted"
      />,
    );
    expect(screen.getByText("Interrupted")).not.toBeNull();
    expect(screen.getByText("Continue")).not.toBeNull();
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
