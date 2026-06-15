import { describe, it, expect, beforeEach } from "vitest";
import { useComposerStore } from "@/store/composer";

describe("composer store", () => {
  beforeEach(() => {
    useComposerStore.setState({ text: "", focusNonce: 0, rejectNonce: 0 });
  });

  it("setText updates the text without bumping the focus nonce", () => {
    useComposerStore.getState().setText("hello");
    expect(useComposerStore.getState().text).toBe("hello");
    expect(useComposerStore.getState().focusNonce).toBe(0);
  });

  it("prefill loads the text and bumps the focus nonce into an empty composer", () => {
    useComposerStore.getState().prefill("edit me");
    expect(useComposerStore.getState().text).toBe("edit me");
    expect(useComposerStore.getState().focusNonce).toBe(1);
  });

  it("bumps the focus nonce on each prefill into an empty composer, even for identical text, so a refocus always fires", () => {
    useComposerStore.getState().prefill("same");
    const first = useComposerStore.getState().focusNonce;
    useComposerStore.setState({ text: "" }); // composer cleared (e.g. sent) between
    useComposerStore.getState().prefill("same");
    expect(useComposerStore.getState().focusNonce).toBe(first + 1);
  });

  it("prefill overwrites a whitespace-only composer (treated as empty)", () => {
    useComposerStore.setState({ text: "   \n " });
    useComposerStore.getState().prefill("edit me");
    expect(useComposerStore.getState().text).toBe("edit me");
    expect(useComposerStore.getState().focusNonce).toBe(1);
    expect(useComposerStore.getState().rejectNonce).toBe(0);
  });

  it("prefill refuses to clobber an in-progress draft, preserving it and bumping rejectNonce", () => {
    useComposerStore.setState({ text: "half-typed draft" });
    useComposerStore.getState().prefill("edit me");
    expect(useComposerStore.getState().text).toBe("half-typed draft"); // preserved
    expect(useComposerStore.getState().rejectNonce).toBe(1);
    expect(useComposerStore.getState().focusNonce).toBe(0); // not a prefill
  });
});
