// @vitest-environment jsdom

import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputBar } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { useSessionModelStore } from "@/store/session-model";
import { useAttachRejectToastStore } from "@/store/attach-reject-toast";
import type { ResolvedModel } from "@/bindings";

const SID = "s1";

// The composer gates attachments off the *resolved model* for this pane (RFC 0005
// §11.3) via the session-model store — caps are derived from `(kind, model)`, no
// longer a per-connection flag. Seed the resolved entry directly, and stub the
// resolver to the same value so the model chip's async load can't clobber the seed.
function seed(supportsVision: boolean, supportsDocuments = false) {
  const resolved: ResolvedModel = {
    connection: "c1",
    model: "m",
    supportsVision,
    supportsDocuments,
    contextWindow: null,
    trainedContextWindow: null,
    contextWindowSource: null,
  };
  vi.spyOn(ipc, "resolveModelSelection").mockResolvedValue(resolved);
  useSessionModelStore.setState({
    resolvedBySession: { [SID]: resolved },
    overrideBySession: {},
    unavailableBySession: {},
  });
  useComposerStore.setState({
    textBySession: {},
    attachmentsBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  useAttachRejectToastStore.setState({ toasts: [] });
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

  it("enqueues a reason-stating reject toast for a gated pick (#723)", () => {
    seed(/* vision */ false, /* documents */ true);
    const { container } = render(<InputBar sessionId={SID} />);

    const input = container.querySelector(
      'input[type="file"]',
    ) as HTMLInputElement;
    fireEvent.change(input, {
      target: { files: [new File(["x"], "a.png", { type: "image/png" })] },
    });

    const toasts = useAttachRejectToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe("This model can't accept images");
  });

  it("renders chips from the composer store (#723)", () => {
    seed(/* vision */ true, /* documents */ true);
    useComposerStore.setState({
      attachmentsBySession: {
        [SID]: [
          {
            kind: "document",
            mediaType: "application/pdf",
            source: { type: "inline", value: "eA==" },
            name: "notes.pdf",
            bytes: 3,
          },
        ],
      },
    });
    render(<InputBar sessionId={SID} />);

    expect(
      screen.getByRole("button", { name: /remove attachment/i }),
    ).toBeTruthy();
  });
});
