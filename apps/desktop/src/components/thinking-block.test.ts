import { describe, expect, it } from "vitest";
import { resolveThinkingOpen } from "@/components/thinking-block";

describe("resolveThinkingOpen", () => {
  it("defaults collapsed when untouched, even mid-stream (#205)", () => {
    expect(resolveThinkingOpen(null)).toBe(false);
  });

  it("respects an explicit user toggle open", () => {
    expect(resolveThinkingOpen(true)).toBe(true);
  });

  it("respects an explicit user toggle closed", () => {
    expect(resolveThinkingOpen(false)).toBe(false);
  });
});
