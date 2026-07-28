import { describe, it, expect } from "vitest";
import {
  resolveLabel,
  filterSessions,
  arrangeSessions,
  groupContentHits,
  sanitizeSnippet,
  selectSessionOverflow,
  formatHitDate,
  SESSION_REVEAL_BATCH,
  stripForkSuffix,
  nextForkTitle,
} from "@/lib/sessions";
import type { Session } from "@/bindings";
import type { SearchHit } from "@/bindings/SearchHit";

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

function hit(sessionId: string, messageId: string): SearchHit {
  return {
    sessionId,
    sessionTitle: null,
    messageId,
    role: "assistant",
    snippet: `<mark>x</mark> in ${messageId}`,
    createdAt: 0,
  };
}

describe("resolveLabel", () => {
  it("prefers the persisted server title over the goal", () => {
    expect(
      resolveLabel(
        session({ id: "1", title: "Server title", goal: "the goal" }),
      ),
    ).toBe("Server title");
  });

  it("falls back to the goal when there is no title", () => {
    expect(resolveLabel(session({ id: "1", goal: "Ship the thing" }))).toBe(
      "Ship the thing",
    );
  });

  it("falls back to the 'New session' string when there is neither", () => {
    expect(resolveLabel(session({ id: "1" }))).toBe("New session");
  });
});

describe("stripForkSuffix", () => {
  it("strips a trailing (Fork k) suffix", () => {
    expect(stripForkSuffix("Refactor auth (Fork 2)")).toBe("Refactor auth");
  });

  it("leaves a title with no fork suffix unchanged", () => {
    expect(stripForkSuffix("Refactor auth")).toBe("Refactor auth");
  });

  it("does not strip a parenthesized suffix that isn't a fork marker", () => {
    expect(stripForkSuffix("Refactor auth (draft)")).toBe(
      "Refactor auth (draft)",
    );
  });
});

describe("nextForkTitle", () => {
  it("starts at (Fork 1) when no forks exist yet", () => {
    expect(nextForkTitle("Refactor auth", ["Refactor auth", "Other"])).toBe(
      "Refactor auth (Fork 1)",
    );
  });

  it("increments past the highest existing (Fork N) sharing the base", () => {
    const existing = ["Refactor auth", "Refactor auth (Fork 1)"];
    expect(nextForkTitle("Refactor auth", existing)).toBe(
      "Refactor auth (Fork 2)",
    );
  });

  it("forking a fork renumbers from the base instead of stacking suffixes", () => {
    const existing = [
      "Refactor auth",
      "Refactor auth (Fork 1)",
      "Refactor auth (Fork 2)",
    ];
    // Forking "Refactor auth (Fork 2)" itself.
    expect(nextForkTitle("Refactor auth (Fork 2)", existing)).toBe(
      "Refactor auth (Fork 3)",
    );
  });

  it("ignores null titles and titles with a different base", () => {
    const existing = [null, "Unrelated (Fork 5)", "Refactor auth (Fork 1)"];
    expect(nextForkTitle("Refactor auth", existing)).toBe(
      "Refactor auth (Fork 2)",
    );
  });

  it("escapes regex-special characters in the base title", () => {
    const existing = ["Fix (a+b)*c [urgent]", "Fix (a+b)*c [urgent] (Fork 1)"];
    expect(nextForkTitle("Fix (a+b)*c [urgent]", existing)).toBe(
      "Fix (a+b)*c [urgent] (Fork 2)",
    );
  });
});

describe("filterSessions", () => {
  const sessions = [
    session({ id: "a", title: "Parser cleanup", goal: "Refactor the parser" }),
    session({ id: "b", goal: "Write the docs" }),
    session({ id: "c" }), // resolves to "New session"
  ];

  it("returns every session for an empty or whitespace query", () => {
    expect(filterSessions(sessions, "")).toHaveLength(3);
    expect(filterSessions(sessions, "   ")).toHaveLength(3);
  });

  it("matches the resolved label (server title), not the raw goal it overrides", () => {
    expect(filterSessions(sessions, "cleanup").map((s) => s.id)).toEqual(["a"]);
    // "refactor" lives in the goal but the title takes precedence, so it must not match.
    expect(filterSessions(sessions, "refactor")).toHaveLength(0);
  });

  it("is case-insensitive and matches a goal label", () => {
    expect(filterSessions(sessions, "DOCS").map((s) => s.id)).toEqual(["b"]);
  });

  it("matches the 'New session' fallback label", () => {
    expect(filterSessions(sessions, "new").map((s) => s.id)).toEqual(["c"]);
  });

  it("returns an empty list when nothing matches", () => {
    expect(filterSessions(sessions, "zzz")).toHaveLength(0);
  });

  it("preserves the original order of matches", () => {
    expect(filterSessions(sessions, "e").map((s) => s.id)).toEqual([
      "a", // Parser cleanup
      "b", // Write the docs
      "c", // New session
    ]);
  });
});

describe("arrangeSessions", () => {
  // Incoming order is backend recency (newest session first).
  const sessions = [
    session({ id: "a" }),
    session({ id: "b" }),
    session({ id: "c" }),
    session({ id: "d" }),
  ];
  const noPins: string[] = [];
  const empty = new Set<string>();

  it("orders pinned first, then live, then dismissed — stable within each group", () => {
    // pinned: d; dismissed: a. Expect d (pinned) → b, c (live) → a (dismissed).
    const out = arrangeSessions(sessions, ["d"], new Set(["a"]));
    expect(out.map((s) => s.id)).toEqual(["d", "b", "c", "a"]);
  });

  it("keeps the original order when nothing is pinned or dismissed", () => {
    expect(arrangeSessions(sessions, noPins, empty).map((s) => s.id)).toEqual([
      "a",
      "b",
      "c",
      "d",
    ]);
  });

  it("sinks dismissed sessions to the bottom, even when pinned", () => {
    // A dismissed session never floats above a live one, pin notwithstanding.
    const out = arrangeSessions(sessions, ["a"], new Set(["a"]));
    expect(out.map((s) => s.id)).toEqual(["b", "c", "d", "a"]);
  });

  it("does not mutate the input array", () => {
    const input = [...sessions];
    arrangeSessions(input, ["d"], new Set(["a"]));
    expect(input.map((s) => s.id)).toEqual(["a", "b", "c", "d"]);
  });

  // #1110: the pinned group used to fall back to incoming recency, so pinning an
  // older session dropped it to the BOTTOM of the pinned group and the Pin click
  // looked like it did nothing.
  it("moves a newly pinned older session to the top of the pinned group", () => {
    // `a` (newest by recency) was pinned first, then `d` (the oldest). Ordering
    // the pinned group by recency would leave `d` at the BOTTOM of it — the bug.
    const out = arrangeSessions(sessions, ["a", "d"], empty);
    expect(out.map((s) => s.id)).toEqual(["d", "a", "b", "c"]);
  });

  it("orders the pinned group newest pin first, not by recency", () => {
    // Pin order a → b → c is the exact reverse of what recency would give.
    expect(arrangeSessions(sessions, ["a", "b", "c"], empty).map((s) => s.id))
      // c pinned last, so it sits on top.
      .toEqual(["c", "b", "a", "d"]);
  });

  it("drops an unpinned session back into the live group, others keep order", () => {
    const before = arrangeSessions(sessions, ["a", "b", "c"], empty);
    expect(before.map((s) => s.id)).toEqual(["c", "b", "a", "d"]);

    // Unpin `b` (session-prefs filters it out of the array).
    const after = arrangeSessions(sessions, ["a", "c"], empty);
    // c still above a; b rejoins the live group in recency position.
    expect(after.map((s) => s.id)).toEqual(["c", "a", "b", "d"]);
  });

  it("ignores pinned ids that aren't in the list", () => {
    // An orphaned pin (session deleted, `purge` not yet run) must not disturb
    // the ordering of the sessions that are present.
    const out = arrangeSessions(sessions, ["gone", "b"], empty);
    expect(out.map((s) => s.id)).toEqual(["b", "a", "c", "d"]);
  });
});

describe("selectSessionOverflow", () => {
  const mk = (n: number) =>
    Array.from({ length: n }, (_, i) =>
      session({ id: `s${i}`, goal: `Session ${i}` }),
    );

  it("shows everything and reports no more when within the reveal count", () => {
    const sessions = mk(10);
    const { visible, hasMore } = selectSessionOverflow(sessions, null, 25);
    expect(visible).toHaveLength(10);
    expect(hasMore).toBe(false);
  });

  it("reveals only the first revealCount rows and flags more remain", () => {
    const sessions = mk(60);
    const { visible, hasMore } = selectSessionOverflow(
      sessions,
      null,
      SESSION_REVEAL_BATCH,
    );
    expect(visible.map((s) => s.id)).toEqual(
      sessions.slice(0, SESSION_REVEAL_BATCH).map((s) => s.id),
    );
    expect(hasMore).toBe(true);
  });

  it("grows by the batch size on each successive reveal count", () => {
    const sessions = mk(60);
    expect(
      selectSessionOverflow(sessions, null, SESSION_REVEAL_BATCH * 2).visible,
    ).toHaveLength(SESSION_REVEAL_BATCH * 2);
    // Third batch exceeds the total, so everything shows and no more remain.
    const third = selectSessionOverflow(
      sessions,
      null,
      SESSION_REVEAL_BATCH * 3,
    );
    expect(third.visible).toHaveLength(60);
    expect(third.hasMore).toBe(false);
  });

  it("always includes the active session even when it falls past the reveal cut", () => {
    const sessions = mk(60);
    const { visible, hasMore } = selectSessionOverflow(
      sessions,
      "s59",
      SESSION_REVEAL_BATCH,
    );
    expect(visible.some((s) => s.id === "s59")).toBe(true);
    // The active row is pulled in on top of the batch, so more still remain.
    expect(hasMore).toBe(true);
  });

  it("does not flag more when the pulled-in active is the only overflow row", () => {
    // Exactly one row past the cut, and it's the active one — pulling it in puts
    // every row on screen, so no "Show more" should render (Abid nit, #669).
    const sessions = mk(SESSION_REVEAL_BATCH + 1);
    const activeId = `s${SESSION_REVEAL_BATCH}`;
    const { visible, hasMore } = selectSessionOverflow(
      sessions,
      activeId,
      SESSION_REVEAL_BATCH,
    );
    expect(visible).toHaveLength(SESSION_REVEAL_BATCH + 1);
    expect(visible.some((s) => s.id === activeId)).toBe(true);
    expect(hasMore).toBe(false);
  });
});

describe("groupContentHits", () => {
  const a = session({ id: "a" });
  const b = session({ id: "b" });
  const c = session({ id: "c" });
  const byId = new Map([a, b, c].map((s) => [s.id, s]));

  it("keeps one row per session, preserving BM25 (input) order", () => {
    const rows = groupContentHits(
      [hit("b", "b1"), hit("a", "a1"), hit("b", "b2"), hit("c", "c1")],
      new Set(),
      byId,
    );
    expect(rows.map((r) => r.session.id)).toEqual(["b", "a", "c"]);
    // The first hit per session wins (best rank).
    expect(rows[0].hit.messageId).toBe("b1");
  });

  it("excludes sessions already shown as title matches", () => {
    const rows = groupContentHits(
      [hit("a", "a1"), hit("b", "b1")],
      new Set(["a"]),
      byId,
    );
    expect(rows.map((r) => r.session.id)).toEqual(["b"]);
  });

  it("drops hits whose session isn't listed (e.g. a draft)", () => {
    const rows = groupContentHits(
      [hit("ghost", "g1"), hit("a", "a1")],
      new Set(),
      byId,
    );
    expect(rows.map((r) => r.session.id)).toEqual(["a"]);
  });

  it("returns an empty list for no hits", () => {
    expect(groupContentHits([], new Set(), byId)).toEqual([]);
  });
});

describe("sanitizeSnippet (#747 C1 — XSS)", () => {
  it("keeps the backend's <mark> delimiters", () => {
    expect(sanitizeSnippet("fix the <mark>parser</mark> bug")).toBe(
      "fix the <mark>parser</mark> bug",
    );
  });

  it("escapes raw HTML in the surrounding message text", () => {
    // An agent message quoting `<img src=x onerror=alert(1)>`, matched on "img".
    const raw = "<mark><img</mark> src=x onerror=alert(1)>";
    const out = sanitizeSnippet(raw);
    expect(out).toBe("<mark>&lt;img</mark> src=x onerror=alert(1)&gt;");
    // No injectable tag survives outside the mark delimiters.
    expect(out).not.toContain("<img");
  });

  it("escapes ampersands and angle brackets, and a bare </script>", () => {
    expect(sanitizeSnippet("a & b <script>x</script>")).toBe(
      "a &amp; b &lt;script&gt;x&lt;/script&gt;",
    );
  });
});

describe("formatHitDate", () => {
  // Wed Jul 10 2026, 15:00 local.
  const now = new Date(2026, 6, 10, 15, 0, 0).getTime();

  it("labels the same calendar day as 'Today', regardless of time of day", () => {
    const earlyThisMorning = new Date(2026, 6, 10, 0, 5, 0).getTime();
    expect(formatHitDate(earlyThisMorning, now)).toBe("Today");
    expect(formatHitDate(now, now)).toBe("Today");
  });

  it("labels the previous calendar day as 'Yesterday', even close to midnight", () => {
    const lateLastNight = new Date(2026, 6, 9, 23, 55, 0).getTime();
    expect(formatHitDate(lateLastNight, now)).toBe("Yesterday");
  });

  it("uses a short month/day date for this year, with no year suffix", () => {
    const earlierThisYear = new Date(2026, 2, 3, 12, 0, 0).getTime();
    expect(formatHitDate(earlierThisYear, now)).toBe("Mar 3");
  });

  it("appends the year when it differs from the current year", () => {
    const lastYear = new Date(2024, 6, 3, 12, 0, 0).getTime();
    expect(formatHitDate(lastYear, now)).toBe("Jul 3, 2024");
  });
});
