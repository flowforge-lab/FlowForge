// @vitest-environment jsdom

import {
  cleanup,
  render,
  screen,
  fireEvent,
  act,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ResponseCopyButton } from "@/components/message-actions";

const ANSWER = "Here is the final answer.\n\n- one\n- two";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("ResponseCopyButton (#604)", () => {
  it("renders a labeled copy button", () => {
    render(<ResponseCopyButton text={ANSWER} />);
    const btn = screen.getByRole("button", { name: /copy/i });
    expect(btn.textContent).toContain("Copy");
  });

  it("copies the exact text to the clipboard and flashes Copied, then reverts", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<ResponseCopyButton text={ANSWER} />);
    const btn = screen.getByRole("button", { name: /copy/i });

    // Click resolves the async writeText, then sets the transient "Copied" state.
    await act(async () => {
      fireEvent.click(btn);
    });
    expect(writeText).toHaveBeenCalledWith(ANSWER);
    expect(btn.textContent).toContain("Copied");

    // Reverts after the 1500ms window.
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    expect(btn.textContent).toContain("Copy");
    expect(btn.textContent).not.toContain("Copied");
  });
});
