// @vitest-environment jsdom

import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputBar } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { useModelConfigStore } from "@/store/model-config";
import type { ProviderRegistry } from "@/bindings";

const SID = "s1";

function registry(
  supportsVision: boolean,
  supportsDocuments: boolean,
): ProviderRegistry {
  return {
    active: "c1",
    connections: [
      {
        id: "c1",
        kind: "candleVllm",
        displayName: "Test",
        model: "m",
        hasKey: false,
        thinking: false,
        reasoningEffort: "medium",
        supportsVision,
        supportsDocuments,
      },
    ],
  };
}

function seed(supportsVision: boolean, supportsDocuments = false) {
  useModelConfigStore.setState({
    registry: registry(supportsVision, supportsDocuments),
  });
  useComposerStore.setState({
    textBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
  });
}

describe("InputBar attach capability gate (#342/#504)", () => {
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

  it("disables and badges the attach button when the model accepts neither images nor documents", () => {
    seed(/* vision */ false, /* documents */ false);
    render(<InputBar sessionId={SID} />);

    const gated = screen.getByRole("button", { name: /unavailable/i });
    expect((gated as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByRole("button", { name: /^Attach image$/ })).toBeNull();
  });

  it("enables the attach button labelled for images when only vision is supported", () => {
    seed(/* vision */ true, /* documents */ false);
    render(<InputBar sessionId={SID} />);

    const attach = screen.getByRole("button", { name: /^Attach image$/ });
    expect((attach as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("button", { name: /unavailable/i })).toBeNull();
  });

  it("enables the attach button labelled for documents when only documents are supported", () => {
    seed(/* vision */ false, /* documents */ true);
    render(<InputBar sessionId={SID} />);

    const attach = screen.getByRole("button", { name: /^Attach document$/ });
    expect((attach as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("button", { name: /unavailable/i })).toBeNull();
  });

  it("labels the attach button for both when images and documents are supported", () => {
    seed(/* vision */ true, /* documents */ true);
    render(<InputBar sessionId={SID} />);

    const attach = screen.getByRole("button", {
      name: /^Attach image or document$/,
    });
    expect((attach as HTMLButtonElement).disabled).toBe(false);
  });

  it("does not stage a picked file while fully gated", () => {
    seed(/* vision */ false, /* documents */ false);
    const { container } = render(<InputBar sessionId={SID} />);

    const input = container.querySelector(
      'input[type="file"]',
    ) as HTMLInputElement;
    const file = new File(["x"], "a.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [file] } });

    expect(
      screen.queryByRole("button", { name: /remove attachment/i }),
    ).toBeNull();
  });

  it("does not stage an image when only documents are supported", () => {
    seed(/* vision */ false, /* documents */ true);
    const { container } = render(<InputBar sessionId={SID} />);

    const input = container.querySelector(
      'input[type="file"]',
    ) as HTMLInputElement;
    const image = new File(["x"], "a.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [image] } });

    expect(
      screen.queryByRole("button", { name: /remove attachment/i }),
    ).toBeNull();
  });
});
