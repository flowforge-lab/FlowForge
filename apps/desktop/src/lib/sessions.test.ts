import { describe, it, expect } from "vitest";
import { resolveLabel, filterSessions, arrangeSessions } from "@/lib/sessions";
import type { Session } from "@/bindings";

function session(partial: Partial<Session> & { id: string }): Session {
  return {
    goal: null,
    title: null,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

describe("resolveLabel", () => {
  it("prefers the persisted server title over everything", () => {
    expect(
      resolveLabel(
        session({ id: "1", title: "Server title", goal: "the goal" }),
        "Legacy local",
      ),
    ).toBe("Server title");
  });

  it("falls back to the legacy localStorage title when the server has none", () => {
    expect(resolveLabel(session({ id: "1", goal: "the goal" }), "Legacy")).toBe(
      "Legacy",
    );
  });

  it("prefers a custom title over the goal", () => {
    expect(resolveLabel(session({ id: "1", goal: "the goal" }), "Custom")).toBe(
      "Custom",
    );
  });

  it("falls back to the goal when there's no custom title", () => {
    expect(
      resolveLabel(session({ id: "1", goal: "Ship the thing" }), undefined),
    ).toBe("Ship the thing");
  });

  it("falls back to 'New session' when there's neither", () => {
    expect(resolveLabel(session({ id: "1" }), undefined)).toBe("New session");
  });
});

describe("filterSessions", () => {
  const sessions = [
    session({ id: "a", goal: "Refactor the parser" }),
    session({ id: "b", goal: "Write the docs" }),
    session({ id: "c" }), // resolves to "New session"
  ];
  // "a" has been renamed, so its label is the custom title, not the goal.
  const titles: Record<string, string> = { a: "Parser cleanup" };

  it("returns every session for an empty or whitespace query", () => {
    expect(filterSessions(sessions, "", titles)).toHaveLength(3);
    expect(filterSessions(sessions, "   ", titles)).toHaveLength(3);
  });

  it("matches the resolved label (custom title), not the raw goal it replaced", () => {
    expect(
      filterSessions(sessions, "cleanup", titles).map((s) => s.id),
    ).toEqual(["a"]);
    // "refactor" lives in the goal but not in the rename, so it must not match.
    expect(filterSessions(sessions, "refactor", titles)).toHaveLength(0);
  });

  it("is case-insensitive and matches a goal label", () => {
    expect(filterSessions(sessions, "DOCS", titles).map((s) => s.id)).toEqual([
      "b",
    ]);
  });

  it("matches the 'New session' fallback label", () => {
    expect(filterSessions(sessions, "new", titles).map((s) => s.id)).toEqual([
      "c",
    ]);
  });

  it("returns an empty list when nothing matches", () => {
    expect(filterSessions(sessions, "zzz", titles)).toHaveLength(0);
  });

  it("preserves the original order of matches", () => {
    expect(filterSessions(sessions, "e", titles).map((s) => s.id)).toEqual([
      "a", // Parser cleanup
      "b", // Write the docs
      "c", // New session
    ]);
  });
});

describe("arrangeSessions", () => {
  const sessions = [
    session({ id: "a" }),
    session({ id: "b" }),
    session({ id: "c" }),
  ];
  const empty = new Set<string>();

  it("floats pinned sessions to the top, stable within each group", () => {
    const out = arrangeSessions(sessions, new Set(["c"]), empty, false);
    expect(out.map((s) => s.id)).toEqual(["c", "a", "b"]);
  });

  it("keeps the original order when nothing is pinned", () => {
    expect(
      arrangeSessions(sessions, empty, empty, false).map((s) => s.id),
    ).toEqual(["a", "b", "c"]);
  });

  it("hides dismissed sessions, and reveals them when showDismissed is set", () => {
    const dismissed = new Set(["b"]);
    expect(
      arrangeSessions(sessions, empty, dismissed, false).map((s) => s.id),
    ).toEqual(["a", "c"]);
    // Restorable: with showDismissed the dismissed session is back in the list.
    expect(
      arrangeSessions(sessions, empty, dismissed, true).map((s) => s.id),
    ).toEqual(["a", "b", "c"]);
  });

  it("does not mutate the input array", () => {
    const input = [...sessions];
    arrangeSessions(input, new Set(["c"]), empty, false);
    expect(input.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });
});
