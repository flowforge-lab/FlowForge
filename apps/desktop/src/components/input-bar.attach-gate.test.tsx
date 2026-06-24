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

function registry(supportsVision: boolean): ProviderRegistry {
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
      },
    ],
  };
}

function seed(supportsVision: boolean) {
  useModelConfigStore.setState({ registry: registry(supportsVision) });
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

describe("InputBar attach vision gate (#342)", () => {
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

  it("disables and badges the attach button when the model lacks vision", () => {
    seed(/* supportsVision */ false);
    render(<InputBar sessionId={SID} />);

    const gated = screen.getByRole("button", { name: /unavailable/i });
    expect((gated as HTMLButtonElement).disabled).toBe(true);
    // The plain enabled affordance is not present while gated.
    expect(screen.queryByRole("button", { name: /^Attach image$/ })).toBeNull();
  });

  it("enables the attach button when the model supports vision", () => {
    seed(/* supportsVision */ true);
    render(<InputBar sessionId={SID} />);

    const attach = screen.getByRole("button", { name: /^Attach image$/ });
    expect((attach as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("button", { name: /unavailable/i })).toBeNull();
  });

  it("does not stage a picked file while gated", () => {
    seed(/* supportsVision */ false);
    const { container } = render(<InputBar sessionId={SID} />);

    const input = container.querySelector(
      'input[type="file"]',
    ) as HTMLInputElement;
    const file = new File(["x"], "a.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [file] } });

    // No chip / remove affordance materializes.
    expect(
      screen.queryByRole("button", { name: /remove attachment/i }),
    ).toBeNull();
  });
});
