import { describe, it, expect, beforeEach } from "vitest";
import { useComposerStore } from "@/store/composer";

describe("composer store", () => {
  beforeEach(() => {
    useComposerStore.setState({ text: "", focusNonce: 0 });
  });

  it("setText updates the text without bumping the focus nonce", () => {
    useComposerStore.getState().setText("hello");
    expect(useComposerStore.getState().text).toBe("hello");
    expect(useComposerStore.getState().focusNonce).toBe(0);
  });

  it("prefill loads the text and bumps the focus nonce", () => {
    useComposerStore.getState().prefill("edit me");
    expect(useComposerStore.getState().text).toBe("edit me");
    expect(useComposerStore.getState().focusNonce).toBe(1);
  });

  it("bumps the nonce on every prefill, even with identical text, so a refocus always fires", () => {
    useComposerStore.getState().prefill("same");
    const first = useComposerStore.getState().focusNonce;
    useComposerStore.getState().prefill("same");
    expect(useComposerStore.getState().focusNonce).toBe(first + 1);
  });
});
