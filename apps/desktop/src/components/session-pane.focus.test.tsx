// @vitest-environment jsdom

import { render, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PaneTree } from "@/components/pane-tree";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePanesStore } from "@/store/panes";

// Clicking a background pane moved the focus ring but dropped keyboard focus to
// <body>, so the next keystroke went nowhere and the user needed a second click on the
// composer (#1122). The real InputBar is rendered here on purpose — the fix is the
// pane → focus-nonce → textarea.focus() chain, and stubbing the composer would test
// nothing. Everything else in the pane is stubbed, as the sibling pane tests do.
vi.mock("@/components/chat-view", () => ({ ChatView: () => null }));
vi.mock("@/components/pheno-selector", () => ({ PhenoSelector: () => null }));
vi.mock("@/components/context-gauge", () => ({ ContextGauge: () => null }));

globalThis.ResizeObserver ||= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
globalThis.requestAnimationFrame ||= ((cb: FrameRequestCallback) =>
  setTimeout(
    () => cb(0),
    0,
  ) as unknown as number) as typeof requestAnimationFrame;

const A = "sess-a";
const B = "sess-b";

// PaneTree, not two bare SessionPanes: `focused` must come from the store, so the
// commit triggered by the mousedown lands before the click handler runs — which is
// exactly the ordering the fix depends on.
const renderPanes = () => render(<PaneTree />);

/** The pane's transcript region — the drop zone wraps exactly the clickable body. */
const paneBody = (container: HTMLElement, index: number) =>
  container.querySelectorAll('[data-testid="pane-dropzone"]')[
    index
  ] as HTMLElement;

const textareaIn = (container: HTMLElement, index: number) =>
  container.querySelectorAll("textarea")[index] as HTMLTextAreaElement;

describe("SessionPane click-to-focus hands over the caret (#1122)", () => {
  beforeEach(() => {
    usePanesStore.setState({
      root: {
        type: "split",
        id: "root",
        dir: "vertical",
        children: [
          { type: "leaf", id: "pane-a", sessionId: A },
          { type: "leaf", id: "pane-b", sessionId: B },
        ],
        ratios: [0.5, 0.5],
      },
      focusedPaneId: "pane-b",
    });
    useChatStore.setState({ activeSessionId: B, sessions: [] });
    useComposerStore.setState({
      textBySession: {},
      focusNonceBySession: {},
      rejectNonceBySession: {},
      attachmentsBySession: {},
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("focuses the pane AND puts the caret in its composer on one click", async () => {
    const user = userEvent.setup();
    const { container } = renderPanes();
    expect(document.activeElement).toBe(textareaIn(container, 1));

    await user.click(paneBody(container, 0));

    expect(usePanesStore.getState().focusedPaneId).toBe("pane-a");
    expect(document.activeElement).toBe(textareaIn(container, 0));
  });

  it("does not steal the caret when a button in the background pane is clicked", async () => {
    const user = userEvent.setup();
    const { container } = renderPanes();

    const filesToggle = container
      .querySelectorAll('[title="Toggle Files (⌘⇧E)"]')
      .item(0) as HTMLElement;
    await user.click(filesToggle);

    // The pane still becomes focused — the mousedown path is untouched — but the
    // click was for the button, so no focus nonce is requested.
    expect(usePanesStore.getState().focusedPaneId).toBe("pane-a");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBeUndefined();
  });

  it("does not request focus when the click ends a text selection", async () => {
    const user = userEvent.setup();
    const { container } = renderPanes();
    vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
    } as Selection);

    await user.click(paneBody(container, 0));

    expect(usePanesStore.getState().focusedPaneId).toBe("pane-a");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBeUndefined();
  });

  it("does not re-request focus when clicking inside the already-focused pane", async () => {
    const user = userEvent.setup();
    const { container } = renderPanes();

    await user.click(paneBody(container, 1));

    expect(usePanesStore.getState().focusedPaneId).toBe("pane-b");
    expect(useComposerStore.getState().focusNonceBySession[B]).toBeUndefined();
  });
});
