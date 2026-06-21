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
// CONTRACT NOTE (#301): mock-only today. There is no backend/ts-rs binding for
// this event yet — `SkillInfo` deliberately omits `mcp`, so the FE cannot derive
// the list itself. The shape mirrors the issue's proposal (`{ phenotype, servers }`)
// and lives here (like SET.5/7's FE-owned types) until the backend emits
// `phenotype:mcp-unavailable` for real. See `ipc.ts` `onPhenotypeMcpUnavailable`.

export interface PhenotypeMcpUnavailableEvent {
  /** The phenotype that was just activated. */
  phenotype: string;
  /** Required-but-unavailable MCP server ids, name-sorted and deduplicated.
   *  Non-empty whenever the event is emitted. */
  servers: string[];
}

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
