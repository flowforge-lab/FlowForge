import { describe, it, expect } from "vitest";
import { parseGoalCommand } from "@/lib/goal-command";

describe("parseGoalCommand (#817)", () => {
  it("parses `/goal <objective>` into a start with the trimmed objective", () => {
    expect(parseGoalCommand("/goal ship the prefix cache PR")).toEqual({
      kind: "start",
      objective: "ship the prefix cache PR",
    });
  });

  it("trims surrounding and inner-edge whitespace around the objective", () => {
    expect(parseGoalCommand("  /goal   refactor the auth layer   ")).toEqual({
      kind: "start",
      objective: "refactor the auth layer",
    });
  });

  it("treats a bare `/goal` as open-dialog (discoverable, not an error)", () => {
    expect(parseGoalCommand("/goal")).toEqual({ kind: "open-dialog" });
    expect(parseGoalCommand("/goal   ")).toEqual({ kind: "open-dialog" });
    expect(parseGoalCommand("   /goal ")).toEqual({ kind: "open-dialog" });
  });

  it("is case-insensitive on the command token", () => {
    expect(parseGoalCommand("/GOAL do the thing")).toEqual({
      kind: "start",
      objective: "do the thing",
    });
  });

  it("keeps a multi-line objective intact", () => {
    expect(parseGoalCommand("/goal line one\nline two")).toEqual({
      kind: "start",
      objective: "line one\nline two",
    });
  });

  it("does NOT match `/goalpost` (token must be delimited)", () => {
    expect(parseGoalCommand("/goalpost is not a goal")).toEqual({
      kind: "not-a-command",
    });
  });

  it("does NOT match `/goal` mid-message or a plain message", () => {
    expect(parseGoalCommand("please /goal later")).toEqual({
      kind: "not-a-command",
    });
    expect(parseGoalCommand("just a normal message")).toEqual({
      kind: "not-a-command",
    });
    expect(parseGoalCommand("")).toEqual({ kind: "not-a-command" });
  });

  it("does NOT match a different slash token", () => {
    expect(parseGoalCommand("/clear")).toEqual({ kind: "not-a-command" });
  });

  it("preserves `/goal` appearing inside the objective text", () => {
    expect(parseGoalCommand("/goal fix the /goal parser edge case")).toEqual({
      kind: "start",
      objective: "fix the /goal parser edge case",
    });
  });
});
