import { describe, expect, it } from "vitest";

import { isWordChar, tokenizeQuery } from "@/lib/find-tokens";

describe("tokenizeQuery (#748)", () => {
  it("splits on whitespace and lowercases", () => {
    expect(tokenizeQuery("Run Turn")).toEqual(["run", "turn"]);
  });

  it("splits on punctuation and underscores", () => {
    expect(tokenizeQuery("run_turn, done!")).toEqual(["run", "turn", "done"]);
  });

  it("de-duplicates repeated tokens", () => {
    expect(tokenizeQuery("run run RUN")).toEqual(["run"]);
  });

  it("keeps letters and digits together", () => {
    expect(tokenizeQuery("gpt4 v11")).toEqual(["gpt4", "v11"]);
  });

  it("returns [] for blank or punctuation-only input", () => {
    expect(tokenizeQuery("")).toEqual([]);
    expect(tokenizeQuery("   ")).toEqual([]);
    expect(tokenizeQuery("--- , .")).toEqual([]);
  });

  it("handles Unicode letters", () => {
    expect(tokenizeQuery("café Über")).toEqual(["café", "über"]);
  });
});

describe("isWordChar (#748)", () => {
  it("is true for letters and digits", () => {
    expect(isWordChar("a")).toBe(true);
    expect(isWordChar("7")).toBe(true);
    expect(isWordChar("é")).toBe(true);
  });

  it("is false for separators and undefined (string boundaries)", () => {
    expect(isWordChar(" ")).toBe(false);
    expect(isWordChar("_")).toBe(false);
    expect(isWordChar(".")).toBe(false);
    expect(isWordChar(undefined)).toBe(false);
  });
});
