// FE surface for the "phenotype skill needs an unavailable MCP server" notice
// (#301). Backs a non-blocking toast that surfaces the warn-only signal PR #296
// added on the backend (`warn_missing_skill_mcp`): when a just-activated
// phenotype lists a skill whose declared MCP server is absent from `mcp.json` or
// present but not running, that skill's bridged tools are silently unavailable
// (its grep/glob fallbacks still work). This carries the list to the UI.
//
// Kept free of React/stores so the copy is unit-testable in vitest's node env
// (mirrors lib/mcp.ts / lib/steps.ts).
//
// The event payload is the ts-rs-generated `PhenotypeMcpUnavailableEvent` binding
// (`crates/ff-core/src/events.rs`, #301); the backend emits it for real from the
// phenotype-switch commands. Imported here only to type the copy helpers below;
// consumers import the type straight from `@/bindings`. See `ipc.ts`
// `onPhenotypeMcpUnavailable`.

import type { PhenotypeMcpUnavailableEvent } from "@/bindings";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

/** Lead sentence naming the phenotype and the unavailable server(s).
 *  Singular/plural aware. */
export function describeUnavailable({
  phenotype,
  servers,
}: PhenotypeMcpUnavailableEvent): string {
  const list = servers.join(", ");
  return servers.length === 1
    ? `${phenotype} needs the ${list} MCP server, which is not available.`
    : `${phenotype} needs ${servers.length} MCP servers (${list}) that are not available.`;
}

/** Full plain-text body the toast renders (the lead sentence plus the reassuring
 *  fallback hint). Singular/plural aware (`it`/`them`). Kept here, not in JSX, so
 *  the copy stays unit-tested. */
export function unavailableToastBody(e: PhenotypeMcpUnavailableEvent): string {
  const what = e.servers.length === 1 ? "it" : "them";
  return `${describeUnavailable(e)} Its grep/glob fallbacks still work — add or start ${what} in MCP settings.`;
}

// Per-server detail line for the sticky notice (#573): the actual spawn/connect
// error when the server is present but failing, a "not configured" note when it is
// absent from `mcp.json` entirely, or a transient-state note otherwise. Keyed off
// the live MCP status snapshot (already in `useMcpStore`), so the notice shows *why*
// each server is unavailable rather than just that it is. Returns one entry per
// server in `e.servers`, preserving order. Pure (no store/React) so the copy stays
// unit-tested.
export function unavailableServerDetails(
  e: PhenotypeMcpUnavailableEvent,
  statusById: Map<string, McpServerStatus>,
): { server: string; detail: string }[] {
  return e.servers.map((server) => {
    const status = statusById.get(server);
    if (!status) {
      return { server, detail: "not configured in mcp.json" };
    }
    if (status.lastError) {
      return { server, detail: status.lastError };
    }
    return { server, detail: `${status.state}, no tools available yet` };
  });
}
