// @vitest-environment jsdom

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SessionItem, SessionMenuItems } from "@/components/session-sidebar";
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

  it("shows a selection checkbox and hides the ⋯ trigger in select mode (#643)", () => {
    const html = render({ selectMode: true });
    expect(html).toContain('aria-label="Select Parser cleanup"');
    // Row actions (⋯ / ⌘ hint) are suppressed while selecting to keep it calm.
    expect(html).not.toContain('aria-label="Session actions"');
  });

  it("reflects the checked state from the `selected` prop (#643)", () => {
    expect(render({ selectMode: true, selected: true })).toContain("checked");
    expect(render({ selectMode: true, selected: false })).not.toContain(
      "checked",
    );
  });
});

// The shared menu body (used by BOTH the right-click ContextMenu and the
// dropdown) is parameterized over the menu primitive's parts, so we render it
// with plain-HTML stand-ins to assert the item set without a portaled menu.
type Parts = Parameters<typeof SessionMenuItems>[0]["parts"];
const PLAIN_PARTS: Parts = {
  Item: ({ className, children }) => (
    <button className={className}>{children}</button>
  ),
  Sub: ({ children }) => <div>{children}</div>,
  SubTrigger: ({ children }) => <div>{children}</div>,
  SubContent: ({ children }) => <div>{children}</div>,
  Separator: () => <hr />,
};

describe("SessionMenuItems", () => {
  function menu(over: Partial<Parameters<typeof SessionMenuItems>[0]> = {}) {
    return renderToStaticMarkup(
      <SessionMenuItems
        parts={PLAIN_PARTS}
        atCap={false}
        pinned={false}
        dismissed={false}
        onOpen={() => {}}
        onOpenSplit={() => {}}
        onTogglePin={() => {}}
        onDismissToggle={() => {}}
        onRename={() => {}}
        onExport={() => {}}
        onDelete={() => {}}
        {...over}
      />,
    );
  }

  it("includes a destructive Delete item alongside the lifecycle actions", () => {
    const html = menu();
    expect(html).toContain("Delete");
    expect(html).toContain("text-destructive");
    // Delete sits with the existing actions, not replacing them.
    expect(html).toContain("Pin");
    expect(html).toContain("Dismiss");
    expect(html).toContain("Rename");
  });
});
