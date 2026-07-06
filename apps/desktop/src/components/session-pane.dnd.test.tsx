// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SessionPane } from "@/components/session-pane";
import { useComposerStore } from "@/store/composer";
import { useSessionModelStore } from "@/store/session-model";
import { useAttachRejectToastStore } from "@/store/attach-reject-toast";
import type { ResolvedModel } from "@/bindings";

// The heavy pane children are irrelevant to the drop contract (staging routes
// straight to the composer store), so stub them to keep the test focused and
// free of their ipc calls.
vi.mock("@/components/chat-view", () => ({ ChatView: () => null }));
vi.mock("@/components/input-bar", () => ({ InputBar: () => null }));
vi.mock("@/components/pheno-selector", () => ({ PhenoSelector: () => null }));
vi.mock("@/components/context-gauge", () => ({ ContextGauge: () => null }));

function resolved(
  supportsVision: boolean,
  supportsDocuments: boolean,
): ResolvedModel {
  return {
    connection: "c1",
    model: "m",
    supportsVision,
    supportsDocuments,
    contextWindow: null,
    trainedContextWindow: null,
    contextWindowSource: null,
  };
}

function seedGate(entries: Record<string, { vision: boolean; docs: boolean }>) {
  const resolvedBySession: Record<string, ResolvedModel> = {};
  for (const [sid, { vision, docs }] of Object.entries(entries)) {
    resolvedBySession[sid] = resolved(vision, docs);
  }
  useSessionModelStore.setState({
    resolvedBySession,
    overrideBySession: {},
    unavailableBySession: {},
  });
}

function fileDrag(files: File[]) {
  return { dataTransfer: { types: ["Files"], files } };
}

function renderPane(sessionId: string) {
  return render(
    <SessionPane
      paneId={`pane-${sessionId}`}
      sessionId={sessionId}
      focused
      canClose
    />,
  );
}

beforeEach(() => {
  useComposerStore.setState({ attachmentsBySession: {} });
  useAttachRejectToastStore.setState({ toasts: [] });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("SessionPane drag-and-drop (#723)", () => {
  it("shows the drop overlay on file drag-over and hides it on leave", () => {
    seedGate({ s1: { vision: true, docs: true } });
    renderPane("s1");
    const zone = screen.getByTestId("pane-dropzone");

    expect(screen.queryByTestId("drop-overlay")).toBeNull();
    fireEvent.dragOver(zone, fileDrag([]));
    expect(screen.queryByTestId("drop-overlay")).not.toBeNull();
    expect(screen.queryByText("Drop files to attach")).not.toBeNull();

    // relatedTarget outside the zone clears it.
    fireEvent.dragLeave(zone, { relatedTarget: document.body });
    expect(screen.queryByTestId("drop-overlay")).toBeNull();
  });

  it("ignores non-file drags (text/selection)", () => {
    seedGate({ s1: { vision: true, docs: true } });
    renderPane("s1");
    fireEvent.dragOver(screen.getByTestId("pane-dropzone"), {
      dataTransfer: { types: ["text/plain"], files: [] },
    });
    expect(screen.queryByTestId("drop-overlay")).toBeNull();
  });

  it("stages every dropped file to this pane's composer", async () => {
    seedGate({ s1: { vision: true, docs: true } });
    renderPane("s1");
    fireEvent.drop(
      screen.getByTestId("pane-dropzone"),
      fileDrag([
        new File(["x"], "a.png", { type: "image/png" }),
        new File(["y"], "b.pdf", { type: "application/pdf" }),
      ]),
    );
    // fileToAttachment reads each file async (FileReader), so wait for staging.
    await waitFor(
      () =>
        expect(
          useComposerStore.getState().attachmentsBySession["s1"] ?? [],
        ).toHaveLength(2),
      { timeout: 3000 },
    );
    expect(useAttachRejectToastStore.getState().toasts).toHaveLength(0);
  });

  it("shows the disabled overlay and stages nothing when the model is gated", () => {
    seedGate({ s1: { vision: false, docs: false } });
    renderPane("s1");
    const zone = screen.getByTestId("pane-dropzone");

    fireEvent.dragOver(zone, fileDrag([]));
    expect(
      screen.queryByText("This model can't accept attachments"),
    ).not.toBeNull();

    fireEvent.drop(
      zone,
      fileDrag([new File(["x"], "a.png", { type: "image/png" })]),
    );
    expect(
      useComposerStore.getState().attachmentsBySession["s1"] ?? [],
    ).toHaveLength(0);
    // The user still gets told why nothing attached.
    expect(useAttachRejectToastStore.getState().toasts[0].message).toBe(
      "This model can't accept images",
    );
  });

  it("routes a drop to the pane under the cursor, not a global target", async () => {
    seedGate({
      s1: { vision: true, docs: true },
      s2: { vision: true, docs: true },
    });
    renderPane("s1");
    const b = renderPane("s2");

    // Bound queries scope to document.body, so scope explicitly to pane B.
    fireEvent.drop(
      within(b.container).getByTestId("pane-dropzone"),
      fileDrag([new File(["x"], "a.png", { type: "image/png" })]),
    );

    await waitFor(() =>
      expect(
        useComposerStore.getState().attachmentsBySession["s2"] ?? [],
      ).toHaveLength(1),
    );
    expect(
      useComposerStore.getState().attachmentsBySession["s1"] ?? [],
    ).toHaveLength(0);
  });
});
