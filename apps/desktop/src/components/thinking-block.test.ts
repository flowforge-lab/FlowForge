import { describe, expect, it } from "vitest";
import { resolveThinkingOpen } from "@/components/thinking-block";

describe("resolveThinkingOpen", () => {
  it("defaults open while reasoning-only is streaming", () => {
    expect(
      resolveThinkingOpen({
        userOpen: null,
        streaming: true,
        hasAnswer: false,
      }),
    ).toBe(true);
  });

  it("defaults collapsed once answer text appears", () => {
    expect(
      resolveThinkingOpen({
        userOpen: null,
        streaming: true,
        hasAnswer: true,
      }),
    ).toBe(false);
  });

  it("defaults collapsed when the turn settles", () => {
    expect(
      resolveThinkingOpen({
        userOpen: null,
        streaming: false,
        hasAnswer: false,
      }),
    ).toBe(false);
  });

  it("respects an explicit user toggle", () => {
    expect(
      resolveThinkingOpen({
        userOpen: true,
        streaming: false,
        hasAnswer: true,
      }),
    ).toBe(true);
  });
});
