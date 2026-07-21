// @vitest-environment jsdom

import {
  cleanup,
  render,
  screen,
  fireEvent,
  act,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  MessageActions,
  ResponseCopyButton,
} from "@/components/message-actions";
import { useChatStore } from "@/store/chat";
import type { Attachment, Message } from "@/bindings";

const ANSWER = "Here is the final answer.\n\n- one\n- two";
const SID = "session-actions";

function msg(over: Partial<Message> = {}): Message {
  return {
    id: "u1",
    sessionId: SID,
    role: "user",
    content: "first prompt",
    createdAt: 1,
    ...over,
  };
}

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

describe("MessageActions — Retry (#929 C)", () => {
  it("offers Retry on the last user message only", () => {
    render(
      <MessageActions message={msg()} side="left" isLastUserMessage={true} />,
    );
    expect(
      screen.getByRole("button", {
        name: /retry — replaces the current answer/i,
      }),
    ).toBeTruthy();
  });

  it("omits Retry on an earlier user message", () => {
    // Retrying mid-conversation would truncate everything after it, so earlier
    // messages stay editable but not retryable.
    render(
      <MessageActions message={msg()} side="left" isLastUserMessage={false} />,
    );
    expect(screen.queryByRole("button", { name: /retry/i })).toBeNull();
    expect(screen.getByRole("button", { name: /edit & resend/i })).toBeTruthy();
  });

  it("omits Retry and Edit on an assistant message", () => {
    render(
      <MessageActions
        message={msg({ id: "a1", role: "assistant", content: ANSWER })}
        side="right"
        isLastUserMessage={true}
      />,
    );
    expect(screen.queryByRole("button", { name: /retry/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /edit/i })).toBeNull();
  });

  it("re-sends the identical content and preserves attachments", () => {
    const editMessage = vi.fn().mockResolvedValue(undefined);
    useChatStore.setState({ editMessage });
    const attachments: Attachment[] = [
      {
        kind: "image",
        mediaType: "image/png",
        source: { type: "path", value: "/tmp/shot.png" },
        name: "shot.png",
        bytes: 3,
      },
    ];

    render(
      <MessageActions
        message={msg({ attachments })}
        side="left"
        isLastUserMessage={true}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /retry/i }));

    // Same content back in — Retry is a re-run, not an edit. Attachments must ride
    // along: the backend replaces that column wholesale, so omitting them destroys
    // them.
    expect(editMessage).toHaveBeenCalledWith(
      SID,
      "u1",
      "first prompt",
      attachments,
    );
  });

  it("Edit opens the bubble-anchored box via onBeginEdit, not the composer", () => {
    const onBeginEdit = vi.fn();
    render(
      <MessageActions
        message={msg()}
        side="left"
        isLastUserMessage={false}
        onBeginEdit={onBeginEdit}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /edit & resend/i }));
    expect(onBeginEdit).toHaveBeenCalled();
  });
});
