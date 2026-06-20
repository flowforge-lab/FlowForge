// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ModePill } from "@/components/input-bar";
import { usePrefsStore } from "@/store/prefs";
import { useSessionModeStore } from "@/store/session-mode";

function label(testId: string): string {
  return (
    within(screen.getByTestId(testId)).getByRole("button").textContent ?? ""
  );
}
function clickPill(testId: string) {
  fireEvent.click(within(screen.getByTestId(testId)).getByRole("button"));
}

describe("ModePill (#266)", () => {
  beforeEach(() => {
    localStorage.clear();
    usePrefsStore.setState({ defaultMode: "auto" });
    useSessionModeStore.setState({ modeBySession: {} });
    render(
      <>
        <div data-testid="a">
          <ModePill sessionId="a" />
        </div>
        <div data-testid="b">
          <ModePill sessionId="b" />
        </div>
      </>,
    );
  });

  afterEach(() => {
    // Unmount the React trees (drops their zustand subscriptions) rather than just
    // wiping the DOM, so state doesn't leak across tests (#287 review).
    cleanup();
  });

  it("shows the default mode and cycles Plan → Act → Auto on click", () => {
    expect(label("a")).toBe("Auto"); // inherits defaultMode

    clickPill("a"); // auto → plan
    expect(label("a")).toBe("Plan");
    clickPill("a"); // plan → act
    expect(label("a")).toBe("Act");
    clickPill("a"); // act → auto
    expect(label("a")).toBe("Auto");
  });

  it("keeps each session's (pane's) mode independent", () => {
    clickPill("a"); // a → plan
    expect(label("a")).toBe("Plan");
    expect(label("b")).toBe("Auto"); // b untouched
  });

  it("persists the per-session mode to localStorage", () => {
    clickPill("a"); // a → plan
    const blob = JSON.parse(localStorage.getItem("ff-session-mode") ?? "{}");
    expect(blob.state.modeBySession.a).toBe("plan");
  });
});
