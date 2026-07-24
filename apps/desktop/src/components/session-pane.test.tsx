// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SessionPane } from "@/components/session-pane";
import { usePanesStore } from "@/store/panes";

// The heavy pane children are irrelevant to the header buttons under test, so
// stub them out (mirrors session-pane.dnd.test.tsx).
vi.mock("@/components/chat-view", () => ({ ChatView: () => null }));
vi.mock("@/components/input-bar", () => ({ InputBar: () => null }));
vi.mock("@/components/pheno-selector", () => ({ PhenoSelector: () => null }));
vi.mock("@/components/context-gauge", () => ({ ContextGauge: () => null }));

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

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

// #1069: the split buttons open a new session (layout gesture), not fork a
// session (content gesture) — fork now lives only in the sidebar.
describe("SessionPane split buttons (#1069)", () => {
  beforeEach(() => {
    usePanesStore.setState({ root: null, focusedPaneId: null });
  });

  it("labels the split buttons as opening a new session, not forking", () => {
    renderPane("s1");
    expect(screen.getByTitle("Open new session right")).not.toBeNull();
    expect(screen.getByTitle("Open new session down")).not.toBeNull();
    expect(screen.queryByTitle(/fork session/i)).toBeNull();
  });

  it("clicking the right-split button calls splitNew, not splitFork", () => {
    const splitNew = vi.fn().mockResolvedValue(undefined);
    usePanesStore.setState({ splitNew });
    renderPane("s1");

    screen.getByTitle("Open new session right").click();

    expect(splitNew).toHaveBeenCalledWith("pane-s1", "vertical");
  });

  it("clicking the down-split button calls splitNew, not splitFork", () => {
    const splitNew = vi.fn().mockResolvedValue(undefined);
    usePanesStore.setState({ splitNew });
    renderPane("s1");

    screen.getByTitle("Open new session down").click();

    expect(splitNew).toHaveBeenCalledWith("pane-s1", "horizontal");
  });

  it("no longer references splitFork anywhere in the store", () => {
    expect(
      (usePanesStore.getState() as unknown as Record<string, unknown>)
        .splitFork,
    ).toBeUndefined();
  });
});
