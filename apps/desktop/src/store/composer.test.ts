import { describe, it, expect, beforeEach } from "vitest";
import { useComposerStore } from "@/store/composer";

const A = "session-a";
const B = "session-b";

describe("composer store (per-session, #148)", () => {
  beforeEach(() => {
    useComposerStore.setState({
      textBySession: {},
      focusNonceBySession: {},
      rejectNonceBySession: {},
      editingBySession: {},
    });
  });

  it("setText updates one session's text without bumping the focus nonce", () => {
    useComposerStore.getState().setText(A, "hello");
    expect(useComposerStore.getState().textBySession[A]).toBe("hello");
    expect(useComposerStore.getState().focusNonceBySession[A] ?? 0).toBe(0);
  });

  it("keeps drafts isolated per session", () => {
    useComposerStore.getState().setText(A, "draft a");
    useComposerStore.getState().setText(B, "draft b");
    expect(useComposerStore.getState().textBySession[A]).toBe("draft a");
    expect(useComposerStore.getState().textBySession[B]).toBe("draft b");
  });

  it("prefill loads the text and bumps the focus nonce into an empty composer", () => {
    useComposerStore.getState().prefill(A, "edit me");
    expect(useComposerStore.getState().textBySession[A]).toBe("edit me");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBe(1);
  });

  it("bumps the focus nonce on each prefill into an empty composer, even for identical text", () => {
    useComposerStore.getState().prefill(A, "same");
    const first = useComposerStore.getState().focusNonceBySession[A];
    useComposerStore.getState().setText(A, ""); // composer cleared (e.g. sent)
    useComposerStore.getState().prefill(A, "same");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBe(first + 1);
  });

  it("prefill overwrites a whitespace-only composer (treated as empty)", () => {
    useComposerStore.getState().setText(A, "   \n ");
    useComposerStore.getState().prefill(A, "edit me");
    expect(useComposerStore.getState().textBySession[A]).toBe("edit me");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBe(1);
    expect(useComposerStore.getState().rejectNonceBySession[A] ?? 0).toBe(0);
  });

  it("prefill refuses to clobber an in-progress draft, preserving it and bumping rejectNonce", () => {
    useComposerStore.getState().setText(A, "half-typed draft");
    useComposerStore.getState().prefill(A, "edit me");
    expect(useComposerStore.getState().textBySession[A]).toBe(
      "half-typed draft",
    );
    expect(useComposerStore.getState().rejectNonceBySession[A]).toBe(1);
    expect(useComposerStore.getState().focusNonceBySession[A] ?? 0).toBe(0);
  });

  it("a draft in one session does not block a prefill into another", () => {
    useComposerStore.getState().setText(A, "busy draft");
    useComposerStore.getState().prefill(B, "edit me");
    expect(useComposerStore.getState().textBySession[B]).toBe("edit me");
    expect(useComposerStore.getState().focusNonceBySession[B]).toBe(1);
    expect(useComposerStore.getState().rejectNonceBySession[B] ?? 0).toBe(0);
  });

  it("beginEdit binds the session to a message id, loads the text, and focuses (#463)", () => {
    useComposerStore.getState().beginEdit(A, "msg-1", "edit me");
    expect(useComposerStore.getState().editingBySession[A]).toBe("msg-1");
    expect(useComposerStore.getState().textBySession[A]).toBe("edit me");
    expect(useComposerStore.getState().focusNonceBySession[A]).toBe(1);
  });

  it("beginEdit sets the text directly over an in-progress draft (targeted, no #48 guard)", () => {
    useComposerStore.getState().setText(A, "half-typed draft");
    useComposerStore.getState().beginEdit(A, "msg-1", "edit me");
    expect(useComposerStore.getState().textBySession[A]).toBe("edit me");
    expect(useComposerStore.getState().editingBySession[A]).toBe("msg-1");
    expect(useComposerStore.getState().rejectNonceBySession[A] ?? 0).toBe(0);
  });

  it("cancelEdit clears the editing binding and the composer text", () => {
    useComposerStore.getState().beginEdit(A, "msg-1", "edit me");
    useComposerStore.getState().cancelEdit(A);
    expect(useComposerStore.getState().editingBySession[A]).toBeUndefined();
    expect(useComposerStore.getState().textBySession[A]).toBe("");
  });
});
