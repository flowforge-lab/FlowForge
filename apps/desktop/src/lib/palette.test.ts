import { describe, it, expect } from "vitest";
import { buildCommands, fuzzyScore, filterCommands } from "@/lib/palette";
import type { Session } from "@/bindings";

function session(partial: Partial<Session> & { id: string }): Session {
  return {
    goal: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

describe("fuzzyScore", () => {
  it("returns null when the query isn't a subsequence", () => {
    expect(fuzzyScore("xyz", "Toggle split panel")).toBeNull();
  });

  it("matches a scattered subsequence", () => {
    expect(fuzzyScore("tsp", "toggle split panel")).not.toBeNull();
  });

  it("is case-insensitive", () => {
    expect(fuzzyScore("SPLIT", "toggle split panel")).toBe(
      fuzzyScore("split", "toggle split panel"),
    );
  });

  it("scores a contiguous, word-boundary match higher than a scattered one", () => {
    const contiguous = fuzzyScore("split", "split")!;
    const scattered = fuzzyScore("split", "sxpxlxixt")!;
    expect(contiguous).toBeGreaterThan(scattered);
  });
});

describe("buildCommands", () => {
  const sessions = [
    session({ id: "a" }), // active
    session({ id: "b", goal: "Write docs" }),
    session({ id: "c" }), // untitled, will be renamed via sessionTitles
  ];
  const built = buildCommands({
    sessions,
    activeSessionId: "a",
    sessionTitles: { c: "Renamed C" },
  });
  const byId = new Map(built.map((c) => [c.id, c] as const));

  it("lists the four quick actions first, in order", () => {
    expect(built.slice(0, 4).map((c) => c.id)).toEqual([
      "action:new-session",
      "action:toggle-split",
      "action:toggle-wrap",
      "action:focus-composer",
    ]);
  });

  it("excludes the active session from the switch list", () => {
    const switchIds = built
      .filter((c) => c.kind === "switch-session")
      .map((c) => c.id);
    expect(switchIds).toEqual(["session:b", "session:c"]); // "a" excluded
  });

  it("labels switch commands via resolveLabel (goal, then custom title)", () => {
    expect(byId.get("session:b")?.title).toBe("Write docs"); // from goal
    expect(byId.get("session:c")?.title).toBe("Renamed C"); // custom title wins
  });

  it("maps the ⌘1–9 hint to each session's index in the FULL list", () => {
    expect(byId.get("session:b")?.hint).toBe("⌘2"); // full-list index 1
    expect(byId.get("session:c")?.hint).toBe("⌘3"); // full-list index 2
  });

  it("shows ⌘9 for the 9th session and drops the hint past it", () => {
    const many = Array.from({ length: 11 }, (_, i) => session({ id: `s${i}` }));
    const builtMany = buildCommands({
      sessions: many,
      activeSessionId: null,
      sessionTitles: {},
    });
    const byIdMany = new Map(builtMany.map((c) => [c.id, c] as const));
    expect(byIdMany.get("session:s8")?.hint).toBe("⌘9"); // index 8
    expect(byIdMany.get("session:s9")?.hint).toBeUndefined(); // index 9 → none
  });
});

describe("filterCommands", () => {
  const commands = buildCommands({
    sessions: [session({ id: "a" }), session({ id: "b", goal: "Write docs" })],
    activeSessionId: "a",
    sessionTitles: {},
  });

  it("orders recently-run commands first on an empty query", () => {
    const ordered = filterCommands(commands, "", ["action:focus-composer"]);
    expect(ordered[0].id).toBe("action:focus-composer");
  });

  it("narrows to fuzzy matches and drops the rest", () => {
    const results = filterCommands(commands, "wrap", []);
    expect(results.map((c) => c.id)).toContain("action:toggle-wrap");
    expect(results.map((c) => c.id)).not.toContain("action:new-session");
  });
});
