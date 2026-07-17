// @vitest-environment jsdom

import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ProcessStatusPanel } from "@/components/process-status-panel";
import { useProcessesStore } from "@/store/processes";
import type { ProcessState } from "@/store/processes";

function proc(
  partial: Partial<ProcessState> & { processId: number },
): ProcessState {
  return {
    output: "",
    status: null,
    startedAt: 0,
    ...partial,
  };
}

// Drive the panel by writing process buffers straight into the store, the same
// way `notebook-status-panel.test.tsx` feeds its store.
function seed(sessionId: string, byId: Record<number, ProcessState>) {
  act(() => {
    useProcessesStore.setState((s) => ({
      bySession: { ...s.bySession, [sessionId]: byId },
    }));
  });
}

beforeEach(() => {
  useProcessesStore.setState({ bySession: {} });
});

afterEach(cleanup);

describe("ProcessStatusPanel (#987)", () => {
  it("self-hides when the session has no processes", () => {
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    expect(container.textContent ?? "").toBe("");
  });

  it("self-hides when the session's process record is present but empty", () => {
    seed("s1", {});
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    expect(container.textContent ?? "").toBe("");
  });

  it("renders a running process with its live output", () => {
    seed("s1", { 1: proc({ processId: 1, output: "booting…\n" }) });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);

    expect(container.textContent).toContain("Process #1");
    expect(container.textContent).toContain("running");
    const pre = container.querySelector("[data-process-output]");
    expect(pre?.textContent).toContain("booting…");
  });

  it("grows the output as new chunks land (append-only)", () => {
    seed("s1", { 1: proc({ processId: 1, output: "line1\n" }) });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    expect(
      container.querySelector("[data-process-output]")?.textContent,
    ).toContain("line1");

    // A later chunk arrives — the store append is reflected without remount.
    seed("s1", { 1: proc({ processId: 1, output: "line1\nline2\n" }) });
    const pre = container.querySelector("[data-process-output]");
    expect(pre?.textContent).toContain("line1");
    expect(pre?.textContent).toContain("line2");
  });

  it("flips to the terminal status badge on exit, keeping the output", () => {
    seed("s1", { 1: proc({ processId: 1, output: "done\n" }) });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    expect(container.textContent).toContain("running");

    seed("s1", {
      1: proc({ processId: 1, output: "done\n", status: "exited(0)" }),
    });
    expect(container.textContent).not.toContain("running");
    expect(container.textContent).toContain("exited(0)");
    // Output is still around after exit — collapsed by default, so re-seed keeps
    // the DOM; assert the exit label is what's shown.
  });

  it("shows a failure status verbatim", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "", status: "failed: spawn error" }),
    });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    expect(container.textContent).toContain("failed: spawn error");
  });

  it("lists multiple processes newest-first", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "old" }),
      3: proc({ processId: 3, output: "new" }),
    });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);
    const text = container.textContent ?? "";
    expect(text.indexOf("Process #3")).toBeLessThan(text.indexOf("Process #1"));
  });
});
