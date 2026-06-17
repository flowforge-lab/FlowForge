// @vitest-environment jsdom

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { McpSection } from "@/components/settings/mcp/mcp-section";
import { ServerRow } from "@/components/settings/mcp/server-row";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

const running: McpServerStatus = {
  id: "github",
  state: "running",
  toolCount: 8,
  restarts: 0,
  pid: 1,
};
const failed: McpServerStatus = {
  id: "playwright",
  state: "failed",
  toolCount: 0,
  lastError: "spawn npx ENOENT",
  restarts: 5,
};

// ServerRow takes its data as a prop, so renderToStaticMarkup shows real content
// (unlike McpSection, whose list comes from the store — zustand's SSR snapshot is
// the initial state, so a seeded list wouldn't render here).
describe("ServerRow", () => {
  it("renders id, state badge, tool count, and actions for a running server", () => {
    const html = renderToStaticMarkup(<ServerRow server={running} />);
    expect(html).toContain("github");
    expect(html).toContain("Running");
    expect(html).toContain("8 tools");
    expect(html).toContain("Restart");
    expect(html).toContain("Disable");
  });

  it("shows the last error and Enable action for a failed/disabled server", () => {
    const failedHtml = renderToStaticMarkup(<ServerRow server={failed} />);
    expect(failedHtml).toContain("Failed");
    expect(failedHtml).toContain("spawn npx ENOENT");
    expect(failedHtml).toContain("5 restarts");

    const disabledHtml = renderToStaticMarkup(
      <ServerRow server={{ ...running, state: "disabled" }} />,
    );
    expect(disabledHtml).toContain("Disabled");
    expect(disabledHtml).toContain("Enable");
  });
});

describe("McpSection", () => {
  it("renders the empty state and the add-server affordance by default", () => {
    // Default store state has no servers (the mount load() effect doesn't run
    // under renderToStaticMarkup).
    const html = renderToStaticMarkup(<McpSection />);
    expect(html).toContain("No MCP servers configured.");
    expect(html).toContain("Add server");
  });
});
