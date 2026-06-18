// @vitest-environment jsdom

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SessionItem } from "@/components/session-sidebar";
import type { Session } from "@/bindings";

// SessionItem takes its row state (session/pinned/dismissed) as props, so static
// markup shows real content — the menu *content* is portaled and only mounts when
// opened, so we assert the row affordances (label, ⋯ trigger, pin glyph) here and
// cover ordering/filtering in lib/sessions.test.ts (arrangeSessions).
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

function render(props: Partial<Parameters<typeof SessionItem>[0]> = {}) {
  return renderToStaticMarkup(
    <SessionItem
      session={session({ id: "a", title: "Parser cleanup" })}
      index={0}
      active={false}
      streaming={false}
      pinned={false}
      dismissed={false}
      {...props}
    />,
  );
}

describe("SessionItem", () => {
  it("renders the label and a ⋯ session-actions trigger", () => {
    const html = render();
    expect(html).toContain("Parser cleanup");
    expect(html).toContain('aria-label="Session actions"');
  });

  it("shows a pin glyph when pinned", () => {
    expect(render({ pinned: false })).not.toContain('aria-label="Pinned"');
    expect(render({ pinned: true })).toContain('aria-label="Pinned"');
  });

  it("dims a dismissed row", () => {
    expect(render({ dismissed: true })).toContain("opacity-60");
  });
});
