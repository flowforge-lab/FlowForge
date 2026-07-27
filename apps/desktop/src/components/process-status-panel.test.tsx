// @vitest-environment jsdom

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
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

// Dismissing finished rows (#1089). A running process must keep its output on
// screen, so the affordances are offered on terminal rows only.
describe("ProcessStatusPanel dismiss affordances (#1089)", () => {
  it("offers [×] on a finished row but not on a running one", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "still going" }),
      2: proc({ processId: 2, output: "done", status: "exited(0)" }),
    });
    render(<ProcessStatusPanel sessionId="s1" />);

    expect(screen.queryByLabelText("Dismiss process 1")).toBeNull();
    expect(screen.getByLabelText("Dismiss process 2")).toBeTruthy();
  });

  it("removes just that row when [×] is clicked", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "still going" }),
      2: proc({ processId: 2, output: "done", status: "exited(0)" }),
    });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);

    fireEvent.click(screen.getByLabelText("Dismiss process 2"));

    expect(container.textContent).not.toContain("Process #2");
    expect(container.textContent).toContain("Process #1");
  });

  it("self-hides once the last process is dismissed", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "done", status: "exited(0)" }),
    });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);

    fireEvent.click(screen.getByLabelText("Dismiss process 1"));

    expect(container.textContent ?? "").toBe("");
  });

  it("counts the finished rows in the header button", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "still going" }),
      2: proc({ processId: 2, status: "exited(0)" }),
      3: proc({ processId: 3, status: "failed: boom" }),
    });
    render(<ProcessStatusPanel sessionId="s1" />);

    expect(screen.getByText("Clear finished (2)")).toBeTruthy();
  });

  it("hides the header entirely while everything is still running", () => {
    seed("s1", { 1: proc({ processId: 1, output: "still going" }) });
    render(<ProcessStatusPanel sessionId="s1" />);

    expect(screen.queryByText(/Clear finished/)).toBeNull();
  });

  it("clears every finished row at once, leaving running ones", () => {
    seed("s1", {
      1: proc({ processId: 1, output: "still going" }),
      2: proc({ processId: 2, status: "exited(0)" }),
      3: proc({ processId: 3, status: "killed" }),
    });
    const { container } = render(<ProcessStatusPanel sessionId="s1" />);

    fireEvent.click(screen.getByText("Clear finished (2)"));

    expect(container.textContent).toContain("Process #1");
    expect(container.textContent).not.toContain("Process #2");
    expect(container.textContent).not.toContain("Process #3");
    // Nothing terminal left, so the header goes with them.
    expect(screen.queryByText(/Clear finished/)).toBeNull();
  });
});
