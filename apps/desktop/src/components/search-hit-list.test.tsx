// @vitest-environment jsdom

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SearchHitList } from "@/components/search-hit-list";
import type { Session } from "@/bindings";
import type { SearchHit } from "@/bindings/SearchHit";
import type { ContentHitRow } from "@/lib/sessions";

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

function hit(partial: Partial<SearchHit> & { messageId: string }): SearchHit {
  return {
    sessionId: "a",
    sessionTitle: null,
    role: "assistant",
    snippet: "…",
    createdAt: 0,
    ...partial,
  };
}

function row(overrides: Partial<ContentHitRow> = {}): ContentHitRow {
  return {
    session: session({ id: "a", title: "Parser cleanup" }),
    hit: hit({ messageId: "m1", snippet: "…fix the <mark>parser</mark> bug…" }),
    ...overrides,
  };
}

function render(props: Partial<Parameters<typeof SearchHitList>[0]> = {}) {
  return renderToStaticMarkup(
    <SearchHitList
      rows={[row()]}
      activeIndex={0}
      onHover={() => {}}
      onSelect={() => {}}
      listRef={{ current: null }}
      variant="dropdown"
      pending={false}
      emptyLabel="No matches"
      {...props}
    />,
  );
}

describe("SearchHitList", () => {
  it("renders the session label and the highlighted snippet markup", () => {
    const html = render();
    expect(html).toContain("Parser cleanup");
    // The snippet's <mark> survives to the DOM (styled by .ff-hit-snippet mark).
    expect(html).toContain("<mark>parser</mark>");
    expect(html).toContain("ff-hit-snippet");
  });

  it("renders a relative date per row", () => {
    const now = Date.now();
    const html = render({
      rows: [row({ hit: hit({ messageId: "m1", createdAt: now }) })],
    });
    expect(html).toContain("Today");
  });

  it("renders the assistant-assisted search row as disabled and non-interactive, without 'Aki'", () => {
    const html = render();
    expect(html).toContain("Search with the agent");
    expect(html).toContain('aria-disabled="true"');
    expect(html).toContain("Coming soon");
    expect(html.toLowerCase()).not.toContain("aki");
  });

  it("highlights the active row via aria-selected", () => {
    const rows = [
      row({ hit: hit({ messageId: "m1" }) }),
      row({ hit: hit({ messageId: "m2" }) }),
    ];
    const html = render({ rows, activeIndex: 1 });
    const options = html.split('role="option"').slice(1);
    expect(options[0]).toMatch(/^[^>]*aria-selected="false"/);
    expect(options[1]).toMatch(/^[^>]*aria-selected="true"/);
  });

  it("shows the empty label only once the search has settled (not while pending)", () => {
    const settled = render({
      rows: [],
      pending: false,
      emptyLabel: "No matches in messages",
    });
    expect(settled).toContain("No matches in messages");

    const pending = render({
      rows: [],
      pending: true,
      emptyLabel: "No matches in messages",
    });
    expect(pending).not.toContain("No matches in messages");
  });
});
