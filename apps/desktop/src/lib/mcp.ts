// Pure MCP helpers — friendly tool-name parsing and per-state badge metadata.
// Kept free of React/stores so they're unit-testable in vitest's node env
// (mirrors lib/palette.ts / lib/steps.ts).

import type { McpServerState } from "@/bindings/McpServerState";

const MCP_PREFIX = "mcp__";

export interface ParsedMcpTool {
  /** The advertising server's id. */
  server: string;
  /** The bare tool name, before `mcp__<server>__` namespacing. */
  tool: string;
}

/**
 * Split a bridged tool id `mcp__<server>__<tool>` into its parts, or `null` for a
 * non-MCP tool. The bare tool name may itself contain `__`, so we split on the
 * first separator after the server segment only.
 */
export function parseMcpToolName(raw: string): ParsedMcpTool | null {
  if (!raw.startsWith(MCP_PREFIX)) return null;
  const rest = raw.slice(MCP_PREFIX.length);
  const sep = rest.indexOf("__");
  if (sep <= 0) return null;
  const server = rest.slice(0, sep);
  const tool = rest.slice(sep + 2);
  if (!server || !tool) return null;
  return { server, tool };
}

export interface McpStateMeta {
  label: string;
  /** Token-only classes for the badge pill. */
  badgeClassName: string;
  /** Token-only classes for the leading status dot. */
  dotClassName: string;
}

// Token-only styling (no new palette): running leans on the accent, failed on
// destructive, the transient/off states stay neutral/muted.
const STATE_META: Record<McpServerState, McpStateMeta> = {
  running: {
    label: "Running",
    badgeClassName: "border-primary/30 bg-primary/10 text-foreground",
    dotClassName: "bg-primary",
  },
  starting: {
    label: "Starting",
    badgeClassName: "border-border bg-muted/50 text-muted-foreground",
    dotClassName: "bg-muted-foreground animate-pulse",
  },
  restarting: {
    label: "Restarting",
    badgeClassName: "border-border bg-muted/50 text-muted-foreground",
    dotClassName: "bg-muted-foreground animate-pulse",
  },
  failed: {
    label: "Failed",
    badgeClassName: "border-destructive/30 bg-destructive/10 text-destructive",
    dotClassName: "bg-destructive",
  },
  disabled: {
    label: "Disabled",
    badgeClassName: "border-border bg-muted/50 text-muted-foreground",
    dotClassName: "bg-muted-foreground/50",
  },
};

export function mcpStateMeta(state: McpServerState): McpStateMeta {
  return STATE_META[state];
}
